use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::{fs, io::Read, path::Path};

use anyhow::Result;
use regex::Regex;

#[derive(Debug, Clone)]
enum EntityType {
    Unknown,
    Class,
    Enum,
    Type,
    Interface,
    Function,
    Const,
}

impl std::fmt::Display for EntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            EntityType::Unknown => write!(f, "unknown"),
            EntityType::Class => write!(f, "class"),
            EntityType::Enum => write!(f, "enum"),
            EntityType::Type => write!(f, "type"),
            EntityType::Interface => write!(f, "interface"),
            EntityType::Function => write!(f, "function"),
            EntityType::Const => write!(f, "const"),
        }
    }
}

#[derive(Debug, Clone)]
struct ImportInfo {
    id: String,
    name: String,
    path: String,
}

impl ImportInfo {
    fn new(name: String, path: String) -> Self {
        let id = generate_entity_id(&path, &name);
        ImportInfo { id, name, path }
    }
}

#[derive(Debug, Clone)]
struct Entity {
    id: String,
    name: String,
    entity_type: EntityType,
    file_path: String,
    deps: Vec<ImportInfo>,
    used: bool,
}

impl Entity {
    fn new(
        name: String,
        entity_type: EntityType,
        file_path: String,
        deps: Vec<ImportInfo>,
    ) -> Self {
        let id = generate_entity_id(&file_path, &name);
        Entity {
            id,
            name,
            entity_type,
            file_path,
            deps,
            used: false,
        }
    }
}

fn scan_and_parse_files(root_path: &Path) -> Option<HashMap<String, Entity>> {
    // Define the directories to scan
    let subdirs = vec!["apps/web", "apps/mobile", "libs"];
    let mut all_files = Vec::new();

    // Collect files from each subdirectory
    for subdir in subdirs {
        let full_path = Path::new(root_path).join(subdir);

        if !full_path.exists() {
            eprintln!(
                "Warning: Directory {:?} does not exist, skipping...",
                full_path
            );
            continue;
        }

        println!("Scanning directory: {:?}", full_path);

        match list_typescript_files(&full_path) {
            Ok(mut files) => {
                println!("  Found {} TypeScript files", files.len());
                all_files.append(&mut files);
            }
            Err(e) => {
                eprintln!("Warning: Could not read directory {:?}: {}", full_path, e);
            }
        }
    }

    if all_files.is_empty() {
        println!("No TypeScript files found in {}", root_path.display());
        return None;
    }

    let mut entities_map: HashMap<String, Entity> = HashMap::new();

    println!("Processing {} TypeScript files...\n", all_files.len());

    for file in &all_files {
        match parse_typescript_file(file, root_path) {
            Ok(result) => {
                // For each import, create an Unknown entity if it doesn't exist
                // and mark existing entities as used
                for import in &result.imports {
                    if let Some(existing) = entities_map.get_mut(&import.id) {
                        existing.used = true;
                    } else {
                        let mut imported_entity = Entity::new(
                            import.name.clone(),
                            EntityType::Unknown,
                            import.path.clone(),
                            Vec::new(),
                        );
                        imported_entity.used = true;
                        entities_map.insert(import.id.clone(), imported_entity);
                    }
                }

                // Process entities from the file
                for entity in result.entities {
                    // If entity already exists, update its type and deps; otherwise insert new
                    if let Some(existing) = entities_map.get_mut(&entity.id) {
                        existing.entity_type = entity.entity_type;
                        existing.deps = entity.deps;
                    } else {
                        entities_map.insert(entity.id.clone(), entity);
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: Could not parse file {}: {}", file, e);
            }
        }
    }

    Some(entities_map)
}

pub fn query_all(root_path: &Path) -> Result<()> {
    if let Some(entities_map) = scan_and_parse_files(root_path) {
        // Display the entities
        println!("Found {} entities:\n", entities_map.len());

        // Sort by UUID for consistent output
        let mut sorted_entities: Vec<_> = entities_map.values().collect();
        sorted_entities.sort_by(|a, b| a.id.cmp(&b.id));

        for entity in sorted_entities {
            println!("ID: {}", entity.id);
            println!("Name: {}", entity.name);
            println!("Type: {}", entity.entity_type);
            println!("File: {}", entity.file_path);
            println!("Deps: {:?}", entity.deps);
            println!("---");
        }

        println!("\nTotal entities in map: {}", entities_map.len());
    }

    Ok(())
}

pub fn query(root_path: &Path, query: &String) -> Result<()> {
    if let Some(entities_map) = scan_and_parse_files(root_path) {
        // Display the entities
        println!("Found {} entities:\n", entities_map.len());

        // Sort by UUID for consistent output
        let mut sorted_entities: Vec<_> = entities_map.values().collect();
        sorted_entities.sort_by(|a, b| a.id.cmp(&b.id));

        for entity in sorted_entities {
            println!("ID: {}", entity.id);
            println!("Name: {}", entity.name);
            println!("Type: {}", entity.entity_type);
            println!("File: {}", entity.file_path);
            println!("---");
        }

        println!("\nTotal entities in map: {}", entities_map.len());

        let entity = query_entity_by_id(&entities_map, query);
        println!("{:?}", entity);
    }

    Ok(())
}

pub fn unused(root_path: &Path) -> Result<()> {
    if let Some(entities_map) = scan_and_parse_files(root_path) {
        // Filter for unused entities (used == false) and exclude Unknown types
        let mut unused_entities: Vec<_> = entities_map
            .values()
            .filter(|e| !e.used && !matches!(e.entity_type, EntityType::Unknown))
            .collect();

        // Sort by file path for consistent output
        unused_entities.sort_by(|a, b| a.file_path.cmp(&b.file_path));

        println!("Found {} unused entities:\n", unused_entities.len());

        for entity in &unused_entities {
            println!("Name: {}", entity.name);
            println!("Type: {}", entity.entity_type);
            println!("File: {}", entity.file_path);
            println!("---");
        }

        println!(
            "\nTotal: {} unused out of {} entities",
            unused_entities.len(),
            entities_map.len()
        );
    }

    Ok(())
}

// Query function to look up an entity by ID
fn query_entity_by_id<'a>(
    entities_map: &'a HashMap<String, Entity>,
    id: &str,
) -> Option<&'a Entity> {
    entities_map.get(id)
}

fn should_skip_directory(dir_name: &str) -> bool {
    matches!(
        dir_name,
        "mocks" | "__mocks__" | "mocks_stubs" | "tests" | "environments" | "i18n"
    )
}

fn should_skip_file(path: &Path) -> bool {
    if let Some(file_name) = path.file_name() {
        if let Some(name_str) = file_name.to_str() {
            return name_str.ends_with(".spec.ts")
                || name_str.ends_with(".d.ts")
                || name_str.ends_with(".stories.ts")
                || name_str.ends_with("-stub.ts")
                || name_str.ends_with("mocks.ts")
                || name_str.ends_with("mock.ts");
        }
    }
    false
}

// Generate a hash-based ID from file_path and name
fn generate_entity_id(file_path: &str, name: &str) -> String {
    let mut hasher = DefaultHasher::new();
    let key = format!("{}:{}", file_path, name);
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn list_typescript_files(dir: &Path) -> Result<Vec<String>> {
    let mut ts_files = Vec::new();

    // Read the directory entries
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Check if we should skip this directory
                if let Some(dir_name) = path.file_name() {
                    if let Some(name_str) = dir_name.to_str() {
                        if should_skip_directory(name_str) {
                            continue; // Skip this directory
                        }
                    }
                }

                // Recursively search subdirectories
                match list_typescript_files(&path) {
                    Ok(mut nested_files) => ts_files.append(&mut nested_files),
                    Err(e) => eprintln!("Warning: Could not read directory {:?}: {}", path, e),
                }
            } else if path.is_file() {
                // Skip .spec.ts and .spec.tsx files
                if should_skip_file(&path) {
                    continue;
                }

                // Check if it's a file and has .ts or .tsx extension
                if let Some(extension) = path.extension() {
                    if extension == "ts" || extension == "tsx" {
                        if let Some(path_str) = path.to_str() {
                            ts_files.push(path_str.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(ts_files)
}

/// Strips single-line (//) and multi-line (/* */) comments from content.
/// Preserves strings so that comment-like patterns inside strings are not stripped.
fn strip_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_string: Option<char> = None;

    while let Some(c) = chars.next() {
        // Handle string literals - don't strip "comments" inside strings
        if in_string.is_none() && (c == '"' || c == '\'' || c == '`') {
            in_string = Some(c);
            result.push(c);
            continue;
        }

        if let Some(quote) = in_string {
            result.push(c);
            // Handle escape sequences
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

        // Handle single-line comments
        if c == '/' {
            if let Some(&next) = chars.peek() {
                if next == '/' {
                    // Skip until end of line
                    chars.next(); // consume second '/'
                    while let Some(&ch) = chars.peek() {
                        if ch == '\n' {
                            break;
                        }
                        chars.next();
                    }
                    continue;
                } else if next == '*' {
                    // Multi-line comment - skip until */
                    chars.next(); // consume '*'
                    while let Some(ch) = chars.next() {
                        if ch == '*' {
                            if let Some(&peek) = chars.peek() {
                                if peek == '/' {
                                    chars.next(); // consume '/'
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
    // Find the position after the keyword
    if let Some(pos) = line.find(keyword) {
        let after_keyword = &line[pos + keyword.len()..];

        // Extract the identifier after the keyword
        let identifier: String = after_keyword
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if !identifier.is_empty() {
            return Some(identifier);
        }
    }
    None
}

fn resolve_import_path(
    importing_file: &str,
    import_source: &str,
    root_path: &Path,
) -> Option<String> {
    let base_path = if import_source.starts_with("@awork/") {
        // Replace @awork/ with {root_path}/libs/shared/src/lib
        let rest = &import_source[7..]; // Skip "@awork/"
        root_path.join("libs/shared/src/lib").join(rest)
    } else if import_source.starts_with("./") || import_source.starts_with("../") {
        // Resolve relative to the importing file's directory
        let importing_dir = Path::new(importing_file).parent()?;
        importing_dir.join(import_source)
    } else {
        // External package - skip
        return None;
    };

    // Try different file extensions
    let extensions = [".ts", ".tsx", "/index.ts", "/index.tsx"];

    for ext in &extensions {
        let full_path = if ext.starts_with('/') {
            // It's a directory index file
            base_path.join(&ext[1..])
        } else {
            // It's a file extension
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

    // Also check if the base path itself exists (e.g., importing './foo.ts' directly)
    if base_path.exists() && base_path.is_file() {
        return base_path
            .canonicalize()
            .ok()?
            .to_str()
            .map(|s| s.to_string());
    }

    // Return the constructed base path even if file doesn't exist
    // This handles cases where the import path is valid but file might not be scanned
    // Always add .ts extension if not already present
    let path_str = base_path.to_string_lossy().to_string();
    if path_str.ends_with(".ts") || path_str.ends_with(".tsx") {
        Some(path_str)
    } else {
        Some(format!("{}.ts", path_str))
    }
}

fn extract_imports(content: &str, file_path: &str, root_path: &Path) -> Vec<ImportInfo> {
    let mut imports = Vec::new();

    // Strip comments first to avoid parsing commented imports
    let content_without_comments = strip_comments(content);

    // Normalize content: collapse multiline imports into single lines
    // This handles imports like:
    // import {
    //   A,
    //   B,
    //   C
    // } from 'path'
    let normalize_re = Regex::new(r#"import\s*\{([^}]*)\}\s*from"#).unwrap();
    let normalized_content =
        normalize_re.replace_all(&content_without_comments, |caps: &regex::Captures| {
            let names = caps[1].replace('\n', " ").replace('\r', " ");
            format!("import {{{}}} from", names)
        });

    // Regex for named imports: import { A, B, C } from 'path'
    let named_import_re = Regex::new(r#"import\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]"#).unwrap();

    // Regex for default imports: import Foo from 'path'
    let default_import_re = Regex::new(r#"import\s+(\w+)\s+from\s*['"]([^'"]+)['"]"#).unwrap();

    // Regex for Angular lazy-loaded imports: import('./path').then(m => m.ModuleName)
    let lazy_import_re = Regex::new(
        r#"import\s*\(\s*['"]([^'"]+)['"]\s*\)\.then\s*\(\s*\w+\s*=>\s*\w+\.(\w+)\s*\)"#,
    )
    .unwrap();

    for cap in named_import_re.captures_iter(&normalized_content) {
        let names_str = &cap[1];
        let import_path = cap[2].to_string();

        // Resolve the import path, skip external packages
        let resolved_path = match resolve_import_path(file_path, &import_path, root_path) {
            Some(path) => path,
            None => continue,
        };

        // Parse each imported name, handling aliases like "Foo as F"
        for name_part in names_str.split(',') {
            let name_part = name_part.trim();
            if name_part.is_empty() {
                continue;
            }

            // Handle "Foo as Bar" - take the original name "Foo"
            let name = if let Some(pos) = name_part.find(" as ") {
                name_part[..pos].trim().to_string()
            } else {
                name_part.to_string()
            };

            imports.push(ImportInfo::new(name, resolved_path.clone()));
        }
    }

    for cap in default_import_re.captures_iter(&normalized_content) {
        let name = cap[1].to_string();
        let import_path = cap[2].to_string();

        // Skip if this looks like a type import or other keyword
        if name == "type" || name == "from" {
            continue;
        }

        // Resolve the import path, skip external packages
        if let Some(resolved_path) = resolve_import_path(file_path, &import_path, root_path) {
            imports.push(ImportInfo::new(name, resolved_path));
        }
    }

    // Handle Angular lazy-loaded imports: loadChildren: () => import('./path').then(m => m.Module)
    for cap in lazy_import_re.captures_iter(&normalized_content) {
        let import_path = cap[1].to_string();
        let name = cap[2].to_string();

        // Resolve the import path, skip external packages
        if let Some(resolved_path) = resolve_import_path(file_path, &import_path, root_path) {
            imports.push(ImportInfo::new(name, resolved_path));
        }
    }

    imports
}

struct FileParseResult {
    entities: Vec<Entity>,
    imports: Vec<ImportInfo>,
}

fn is_entity_used_locally(content: &str, entity_name: &str) -> bool {
    // Create a regex pattern that matches the entity name as a whole word
    // This avoids matching substrings (e.g., "Foo" in "FooBar")
    let pattern = format!(r"\b{}\b", regex::escape(entity_name));
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return false,
    };

    // Count occurrences - if more than 1, it's used locally (1 = export definition)
    let matches: Vec<_> = re.find_iter(content).collect();
    matches.len() > 1
}

fn parse_typescript_file(file_path: &str, root_path: &Path) -> Result<FileParseResult> {
    let mut file = fs::File::open(file_path)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;

    let mut entities = Vec::new();

    // Extract all imports from the file (shared by all entities in this file)
    let deps = extract_imports(&content, file_path, root_path);

    // Strip comments before parsing exports
    let content_without_comments = strip_comments(&content);

    for line in content_without_comments.lines() {
        let trimmed = line.trim();

        // Skip empty lines
        if trimmed.is_empty() {
            continue;
        }

        // Check for exported classes
        if trimmed.contains("export") && trimmed.contains("class") {
            if let Some(name) = extract_export_name(trimmed, "class") {
                entities.push(Entity::new(
                    name,
                    EntityType::Class,
                    file_path.to_string(),
                    deps.clone(),
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
                    deps.clone(),
                ));
            }
        }

        // Check for exported types
        if trimmed.contains("export") && trimmed.contains("type") && !trimmed.contains("typeof") {
            if let Some(name) = extract_export_name(trimmed, "type") {
                entities.push(Entity::new(
                    name,
                    EntityType::Type,
                    file_path.to_string(),
                    deps.clone(),
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
                    deps.clone(),
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
                    deps.clone(),
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
                // Consider it a function if the line contains => or = function
                if trimmed.contains("=>") || trimmed.contains("= function") {
                    entities.push(Entity::new(
                        name,
                        EntityType::Function,
                        file_path.to_string(),
                        deps.clone(),
                    ));
                } else {
                    entities.push(Entity::new(
                        name,
                        EntityType::Const,
                        file_path.to_string(),
                        deps.clone(),
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

    Ok(FileParseResult {
        entities,
        imports: deps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_extract_single_named_import() {
        let content = r#"import { Foo } from './foo';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/bar.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Foo");
        assert!(imports[0].path.contains("foo"));
    }

    #[test]
    fn test_extract_multiple_named_imports() {
        let content = r#"import { Foo, Bar, Baz } from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
        assert_eq!(imports[2].name, "Baz");
    }

    #[test]
    fn test_extract_multiline_named_imports() {
        let content = r#"import {
  Foo,
  Bar,
  Baz
} from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
        assert_eq!(imports[2].name, "Baz");
    }

    #[test]
    fn test_extract_aliased_import() {
        let content = r#"import { Foo as F, Bar as B } from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_extract_default_import() {
        let content = r#"import Foo from './foo';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/bar.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Foo");
    }

    #[test]
    fn test_extract_awork_alias_import() {
        let content = r#"import { Model } from '@awork/models';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/apps/web/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Model");
        assert!(imports[0].path.contains("libs/shared/src/lib"));
        assert!(imports[0].path.contains("models"));
        assert!(!imports[0].path.contains("@awork"));
    }

    #[test]
    fn test_skip_external_package_imports() {
        let content = r#"import { useState } from 'react';
import { Observable } from 'rxjs';
import { Foo } from './local';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Foo");
    }

    #[test]
    fn test_extract_multiple_import_statements() {
        let content = r#"import { Foo } from './foo';
import { Bar, Baz } from './bar';
import Default from './default';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 4);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
        assert_eq!(imports[2].name, "Baz");
        assert_eq!(imports[3].name, "Default");
    }

    #[test]
    fn test_extract_relative_parent_path_import() {
        let content = r#"import { Util } from '../utils/helper';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/components/button.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Util");
        assert!(imports[0].path.contains("utils"));
        assert!(imports[0].path.contains("helper"));
    }

    #[test]
    fn test_import_path_gets_ts_extension() {
        let content = r#"import { Foo } from './foo';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/bar.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert!(imports[0].path.ends_with(".ts"));
    }

    #[test]
    fn test_multiline_import_with_trailing_comma() {
        let content = r#"import {
  Foo,
  Bar,
} from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_import_info_has_id() {
        let content = r#"import { Foo } from './foo';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/bar.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert!(!imports[0].id.is_empty());
    }

    #[test]
    fn test_strip_single_line_comment() {
        let content = "const a = 1; // this is a comment\nconst b = 2;";
        let result = strip_comments(content);
        assert_eq!(result, "const a = 1; \nconst b = 2;");
    }

    #[test]
    fn test_strip_multiline_comment() {
        let content = "const a = 1; /* this is\na multiline\ncomment */ const b = 2;";
        let result = strip_comments(content);
        assert_eq!(result, "const a = 1;  const b = 2;");
    }

    #[test]
    fn test_strip_full_line_comment() {
        let content = "// full line comment\nconst a = 1;";
        let result = strip_comments(content);
        assert_eq!(result, "\nconst a = 1;");
    }

    #[test]
    fn test_preserve_string_with_comment_like_content() {
        let content = r#"const a = "// not a comment";"#;
        let result = strip_comments(content);
        assert_eq!(result, r#"const a = "// not a comment";"#);
    }

    #[test]
    fn test_preserve_string_with_multiline_comment_like_content() {
        let content = r#"const a = "/* not a comment */";"#;
        let result = strip_comments(content);
        assert_eq!(result, r#"const a = "/* not a comment */";"#);
    }

    #[test]
    fn test_skip_commented_import() {
        let content = r#"// import { Foo } from './foo';
import { Bar } from './bar';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Bar");
    }

    #[test]
    fn test_skip_multiline_commented_import() {
        let content = r#"/* import { Foo } from './foo'; */
import { Bar } from './bar';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Bar");
    }

    #[test]
    fn test_skip_import_inside_multiline_comment() {
        let content = r#"/*
import { Foo } from './foo';
import { Baz } from './baz';
*/
import { Bar } from './bar';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Bar");
    }

    #[test]
    fn test_extract_angular_lazy_loaded_import() {
        let content = r#"const routes: Routes = [
    {
        path: 'auth',
        loadChildren: () => import('./auth/auth.module').then(m => m.AuthModule)
    }
];"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/app-routing.module.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "AuthModule");
        assert!(imports[0].path.contains("auth/auth.module"));
    }

    #[test]
    fn test_extract_multiple_angular_lazy_loaded_imports() {
        let content = r#"const routes: Routes = [
    {
        path: 'auth',
        loadChildren: () => import('./auth/auth.module').then(m => m.AuthModule)
    },
    {
        path: 'dashboard',
        loadChildren: () => import('./dashboard/dashboard.module').then(mod => mod.DashboardModule)
    }
];"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/app-routing.module.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "AuthModule");
        assert_eq!(imports[1].name, "DashboardModule");
    }

    #[test]
    fn test_extract_lazy_import_with_different_param_names() {
        let content = r#"loadChildren: () => import('./users/users.module').then(module => module.UsersModule)"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/app-routing.module.ts";

        let imports = extract_imports(content, file_path, root_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "UsersModule");
    }
}
