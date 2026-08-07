mod entity;
mod git;
mod graph;
mod mem_leaks;
mod parser;
mod scanner;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::rc::Rc;

use anyhow::Result;

use entity::{Entity, EntityType};
use git::{ChangeType, ChangedFile, get_changed_files};
use graph::DependencyGraph;
use mem_leaks::analyze_and_print as analyze_mem_leaks_and_print;
use parser::Parser;
use scanner::Scanner;

fn is_test_file(path: &str) -> bool {
    path.ends_with(".test.ts") || path.ends_with(".spec.ts") || path.ends_with(".e2e.ts")
}

fn find_test_files_in_directories(directories: &HashSet<String>) -> Vec<String> {
    let mut test_files: HashSet<String> = HashSet::new();

    for dir_path in directories {
        let dir = Path::new(dir_path);
        if !dir.is_dir() {
            continue;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(path_str) = path.to_str() {
                        if is_test_file(path_str) {
                            test_files.insert(path_str.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut sorted: Vec<String> = test_files.into_iter().collect();
    sorted.sort();
    sorted
}

struct ScanResult {
    entities: HashMap<String, Entity>,
}

fn scan_and_parse_files(root_path: &Path, verbose: bool) -> Result<ScanResult> {
    let subdirs = ["apps/web", "apps/mobile", "libs"];
    let mut all_files = Vec::new();

    let scanner = Scanner::new();

    for subdir in subdirs {
        let full_path = root_path.join(subdir);

        if !full_path.exists() {
            if verbose {
                eprintln!(
                    "Warning: Directory {:?} does not exist, skipping...",
                    full_path
                );
            }
            continue;
        }

        if verbose {
            println!("Scanning directory: {:?}", full_path);
        }

        match scanner.scan(&full_path) {
            Ok(mut files) => {
                if verbose {
                    println!("  Found {} TypeScript files", files.len());
                }
                all_files.append(&mut files);
            }
            Err(e) => {
                if verbose {
                    eprintln!("Warning: Could not read directory {:?}: {}", full_path, e);
                }
            }
        }
    }

    if all_files.is_empty() {
        anyhow::bail!("No TypeScript files found in {}", root_path.display());
    }

    let mut entities_map: HashMap<String, Entity> = HashMap::new();

    if verbose {
        println!("Processing {} TypeScript files...\n", all_files.len());
    }

    let parser = Parser::new(root_path);

    for file in &all_files {
        match parser.parse(file) {
            Ok(result) => {
                for import in &result.imports {
                    if let Some(existing) = entities_map.get_mut(&import.id) {
                        existing.used = true;
                    } else {
                        let mut imported_entity = Entity::new(
                            import.name.clone(),
                            EntityType::Unknown,
                            import.path.clone(),
                            Rc::new(Vec::new()),
                        );
                        imported_entity.used = true;
                        entities_map.insert(import.id.clone(), imported_entity);
                    }
                }

                for entity in result.entities {
                    if let Some(existing) = entities_map.get_mut(&entity.id) {
                        existing.entity_type = entity.entity_type;
                        existing.deps = entity.deps;
                    } else {
                        entities_map.insert(entity.id.clone(), entity);
                    }
                }
            }
            Err(e) => {
                if verbose {
                    eprintln!("Warning: Could not parse file {}: {}", file, e);
                }
            }
        }
    }

    Ok(ScanResult {
        entities: entities_map,
    })
}

fn print_entity(entity: &Entity, show_id: bool, show_deps: bool) {
    if show_id {
        println!("ID: {}", entity.id);
    }
    println!("Name: {}", entity.name);
    println!("Type: {}", entity.entity_type);
    println!("File: {}", entity.file_path);
    if show_deps {
        println!("Deps: {:?}", entity.deps);
    }
    println!("---");
}

pub fn query_all(root_path: &Path) -> Result<()> {
    let result = scan_and_parse_files(root_path, true)?;

    println!("Found {} entities:\n", result.entities.len());

    let mut sorted_entities: Vec<_> = result.entities.values().collect();
    sorted_entities.sort_by(|a, b| a.id.cmp(&b.id));

    for entity in sorted_entities {
        print_entity(entity, true, true);
    }

    println!("\nTotal entities in map: {}", result.entities.len());

    Ok(())
}

pub fn query(root_path: &Path, query: &str) -> Result<()> {
    let result = scan_and_parse_files(root_path, false)?;

    if let Some(entity) = result.entities.get(query) {
        print_entity(entity, true, true);
    } else {
        println!("Entity not found: {}", query);
    }

    Ok(())
}

pub fn unused(root_path: &Path, fail_on_findings: bool) -> Result<()> {
    let result = scan_and_parse_files(root_path, true)?;

    let mut unused_entities: Vec<_> = result
        .entities
        .values()
        .filter(|e| !e.used && !matches!(e.entity_type, EntityType::Unknown))
        .collect();

    unused_entities.sort_by(|a, b| a.file_path.cmp(&b.file_path));

    println!("Found {} unused entities:\n", unused_entities.len());

    for entity in &unused_entities {
        print_entity(entity, false, false);
    }

    println!(
        "\nTotal: {} unused out of {} entities",
        unused_entities.len(),
        result.entities.len()
    );

    if fail_on_findings && !unused_entities.is_empty() {
        anyhow::bail!(
            "Found {} unused entities (failing due to --fail-on-findings)",
            unused_entities.len()
        );
    }

    Ok(())
}

pub fn graph_json(root_path: &Path, entity_type_filters: &[String]) -> Result<String> {
    let result = scan_and_parse_files(root_path, false)?;

    let filtered_entities = if entity_type_filters.is_empty() {
        result.entities
    } else {
        result
            .entities
            .into_iter()
            .filter(|(_, entity)| {
                let type_str = entity.entity_type.to_string();
                entity_type_filters.contains(&type_str)
            })
            .collect()
    };

    let graph = DependencyGraph::from_entities(&filtered_entities);
    let json = graph.to_json()?;
    Ok(json)
}

fn matches_project_filter(file_path: &str, project_filter: Option<&str>) -> bool {
    match project_filter {
        None => true,
        Some(pattern) => file_path.contains(pattern),
    }
}

pub fn affected(
    root_path: &Path,
    base_ref: &str,
    transitive: bool,
    paths_only: bool,
    tests_only: bool,
    summary: bool,
    project_filter: Option<&str>,
) -> Result<()> {
    if summary {
        println!("Analyzing changes between HEAD and '{}'...\n", base_ref);
    }

    let changed_files = get_changed_files(root_path, base_ref)?;

    if changed_files.is_empty() {
        if summary {
            println!("No changes found between HEAD and '{}'.", base_ref);
        }
        return Ok(());
    }

    if summary {
        println!("Changed files ({}):", changed_files.len());
        for cf in &changed_files {
            println!("  [{}] {}", cf.change_type, cf.path);
        }
        println!();
    }

    let result = scan_and_parse_files(root_path, false)?;

    let graph = DependencyGraph::from_entities(&result.entities);

    let changed_paths: HashSet<String> = changed_files.iter().map(|cf| cf.path.clone()).collect();

    let mut direct_affected: Vec<(&Entity, &ChangedFile)> = Vec::new();
    let mut direct_affected_ids: HashSet<String> = HashSet::new();

    for entity in result.entities.values() {
        if changed_paths.contains(&entity.file_path)
            && matches_project_filter(&entity.file_path, project_filter)
        {
            if let Some(cf) = changed_files.iter().find(|cf| cf.path == entity.file_path) {
                direct_affected.push((entity, cf));
                direct_affected_ids.insert(entity.id.clone());
            }
        }
    }

    direct_affected.sort_by(|a, b| a.0.file_path.cmp(&b.0.file_path));

    let consumer_ids = graph.find_consumers(&direct_affected_ids, transitive);

    let mut consumers: Vec<(&Entity, String)> = Vec::new();
    for consumer_id in &consumer_ids {
        if let Some(entity) = result.entities.get(consumer_id) {
            if !matches_project_filter(&entity.file_path, project_filter) {
                continue;
            }
            let consumes: Vec<String> = entity
                .deps
                .iter()
                .filter_map(|dep| {
                    for (affected_entity, _) in &direct_affected {
                        if affected_entity.file_path == dep.path && affected_entity.name == dep.name
                        {
                            return Some(affected_entity.name.clone());
                        }
                    }
                    None
                })
                .collect();

            let reason = if consumes.is_empty() {
                "Transitive dependency".to_string()
            } else {
                format!("Imports: {}", consumes.join(", "))
            };

            consumers.push((entity, reason));
        }
    }

    consumers.sort_by(|a, b| a.0.file_path.cmp(&b.0.file_path));

    if tests_only {
        let mut test_files: HashSet<String> = HashSet::new();

        // Collect directories from directly affected entities
        let mut affected_dirs: HashSet<String> = HashSet::new();
        for (entity, _) in &direct_affected {
            if let Some(parent) = Path::new(&entity.file_path).parent() {
                affected_dirs.insert(parent.to_string_lossy().to_string());
            }
        }

        // Collect directories from consumer entities
        for (entity, _) in &consumers {
            if let Some(parent) = Path::new(&entity.file_path).parent() {
                affected_dirs.insert(parent.to_string_lossy().to_string());
            }
        }

        // Find test files in those directories
        let discovered_tests = find_test_files_in_directories(&affected_dirs);
        for test_path in discovered_tests {
            test_files.insert(test_path);
        }

        // Include test files that were directly changed in the git diff
        for cf in &changed_files {
            if is_test_file(&cf.path) && matches_project_filter(&cf.path, project_filter) {
                test_files.insert(cf.path.clone());
            }
        }

        // Output sorted test file paths
        let mut sorted_tests: Vec<String> = test_files.into_iter().collect();
        sorted_tests.sort();

        for test_path in sorted_tests {
            println!("{}", test_path);
        }

        return Ok(());
    }

    if summary {
        println!("---");
        println!("Directly affected entities ({}):\n", direct_affected.len());

        for (entity, cf) in &direct_affected {
            print_affected_entity(
                entity,
                &format!("{} file", change_type_to_reason(&cf.change_type)),
            );
        }

        if !consumers.is_empty() {
            println!("Consumer entities ({}):\n", consumers.len());

            for (entity, reason) in &consumers {
                print_affected_entity(entity, reason);
            }
        }

        let total = direct_affected.len() + consumers.len();
        println!(
            "Summary: {} changed files, {} direct, {} consumers, {} total affected",
            changed_files.len(),
            direct_affected.len(),
            consumers.len(),
            total
        );
    } else if paths_only {
        let mut unique_dirs: HashSet<String> = HashSet::new();

        for (entity, _) in &direct_affected {
            if let Some(parent) = Path::new(&entity.file_path).parent() {
                unique_dirs.insert(parent.to_string_lossy().to_string());
            }
        }

        for (entity, _) in &consumers {
            if let Some(parent) = Path::new(&entity.file_path).parent() {
                unique_dirs.insert(parent.to_string_lossy().to_string());
            }
        }

        let mut sorted_dirs: Vec<_> = unique_dirs.into_iter().collect();
        sorted_dirs.sort();

        for dir in sorted_dirs {
            println!("{}", dir);
        }
    } else {
        let mut affected_files: HashSet<String> = HashSet::new();

        for (entity, _) in &direct_affected {
            affected_files.insert(entity.file_path.clone());
        }

        for (entity, _) in &consumers {
            affected_files.insert(entity.file_path.clone());
        }

        let mut sorted_files: Vec<_> = affected_files.into_iter().collect();
        sorted_files.sort();

        for file_path in sorted_files {
            println!("{}", file_path);
        }
    }

    Ok(())
}

fn print_affected_entity(entity: &Entity, reason: &str) {
    println!("Name: {}", entity.name);
    println!("Type: {}", entity.entity_type);
    println!("File: {}", entity.file_path);
    println!("Reason: {}", reason);
    println!("---");
}

fn change_type_to_reason(change_type: &ChangeType) -> &'static str {
    match change_type {
        ChangeType::Added => "New",
        ChangeType::Modified => "Modified",
        ChangeType::Deleted => "Deleted",
        ChangeType::Renamed => "Renamed",
    }
}

pub fn chain(
    root_path: &Path,
    start_name: &str,
    end_name: &str,
    shortest: bool,
    max_paths: usize,
    max_depth: usize,
) -> Result<()> {
    let result = scan_and_parse_files(root_path, false)?;
    let graph = DependencyGraph::from_entities(&result.entities);

    // Find entity IDs by exact name match
    let start_matches: Vec<&Entity> = result
        .entities
        .values()
        .filter(|e| e.name == start_name)
        .collect();

    let end_matches: Vec<&Entity> = result
        .entities
        .values()
        .filter(|e| e.name == end_name)
        .collect();

    // Validate entities exist
    if start_matches.is_empty() {
        anyhow::bail!("Entity '{}' not found", start_name);
    }
    if end_matches.is_empty() {
        anyhow::bail!("Entity '{}' not found", end_name);
    }

    // Find chains between all matching start and end entities
    let mut found_any = false;
    let mut total_paths = 0;

    for start_entity in &start_matches {
        for end_entity in &end_matches {
            if shortest {
                // Only find the shortest path
                if let Some(path_ids) = graph.find_path(&start_entity.id, &end_entity.id) {
                    let names: Vec<String> = path_ids
                        .iter()
                        .filter_map(|id| result.entities.get(id).map(|e| e.name.clone()))
                        .collect();
                    println!("{}", names.join(" -> "));
                    found_any = true;
                }
            } else {
                // Find all paths (up to remaining max)
                let remaining = max_paths.saturating_sub(total_paths);
                if remaining == 0 {
                    break;
                }
                let all_paths =
                    graph.find_all_paths(&start_entity.id, &end_entity.id, remaining, max_depth);
                for path_ids in all_paths {
                    let names: Vec<String> = path_ids
                        .iter()
                        .filter_map(|id| result.entities.get(id).map(|e| e.name.clone()))
                        .collect();
                    println!("{}", names.join(" -> "));
                    found_any = true;
                    total_paths += 1;
                }
            }
        }
        if !shortest && total_paths >= max_paths {
            break;
        }
    }

    if !found_any {
        println!(
            "No dependency chain found between '{}' and '{}'",
            start_name, end_name
        );
    } else if !shortest && total_paths >= max_paths {
        eprintln!(
            "Note: Output limited to {} paths. Use --max-paths to adjust.",
            max_paths
        );
    }

    Ok(())
}

pub fn cycles(root_path: &Path, max_cycles: usize, max_depth: usize) -> Result<()> {
    let result = scan_and_parse_files(root_path, false)?;
    let graph = DependencyGraph::from_entities(&result.entities);

    let cycles = graph.find_cycles(max_cycles, max_depth);

    if cycles.is_empty() {
        println!("No circular dependencies detected.");
        return Ok(());
    }

    println!("Found {} circular dependencies:\n", cycles.len());

    for (i, cycle) in cycles.iter().enumerate() {
        println!("Cycle {} ({} entities):", i + 1, cycle.len());

        // Build the cycle display with entity names
        let names: Vec<String> = cycle
            .iter()
            .filter_map(|id| result.entities.get(id).map(|e| e.name.clone()))
            .collect();

        // Add first name again to show the cycle closes
        let mut display_names = names.clone();
        if let Some(first) = names.first() {
            display_names.push(first.clone());
        }
        println!("  {}", display_names.join(" -> "));

        // Show file paths
        println!("  Files:");
        for id in cycle {
            if let Some(entity) = result.entities.get(id) {
                println!("    {}", entity.file_path);
            }
        }
        println!("---");
    }

    let limited = cycles.len() >= max_cycles;
    if limited {
        eprintln!(
            "Note: Output limited to {} cycles. Use --max-cycles to adjust.",
            max_cycles
        );
    }

    println!("\nSummary: {} cycles detected", cycles.len());

    Ok(())
}

pub fn rank_by_deps(root_path: &Path, entity_type_filters: &[String]) -> Result<()> {
    let result = scan_and_parse_files(root_path, false)?;

    let filtered_entities = if entity_type_filters.is_empty() {
        result.entities
    } else {
        result
            .entities
            .into_iter()
            .filter(|(_, entity)| {
                let type_str = entity.entity_type.to_string();
                entity_type_filters.contains(&type_str)
            })
            .collect()
    };

    let graph = DependencyGraph::from_entities(&filtered_entities);

    // Count outgoing edges (dependencies) per entity
    let mut dep_counts: HashMap<String, usize> = HashMap::new();
    for node in &graph.nodes {
        dep_counts.insert(node.id.clone(), 0);
    }
    for edge in &graph.edges {
        *dep_counts.entry(edge.source.clone()).or_insert(0) += 1;
    }

    // Build list of (count, node) and sort by count ascending
    let mut ranked: Vec<(usize, &graph::GraphNode)> = graph
        .nodes
        .iter()
        .map(|node| {
            let count = dep_counts.get(&node.id).copied().unwrap_or(0);
            (count, node)
        })
        .collect();

    ranked.sort_by_key(|(count, node)| (*count, node.name.clone()));

    // Output tab-separated: count, name, type, file
    for (count, node) in ranked {
        println!(
            "{}\t{}\t{}\t{}",
            count, node.name, node.entity_type, node.file
        );
    }

    Ok(())
}

pub fn mem_leaks(
    root_path: &Path,
    entity_type_filters: &[String],
    max_findings: usize,
    strict: bool,
) -> Result<()> {
    let result = scan_and_parse_files(root_path, false)?;
    analyze_mem_leaks_and_print(&result.entities, entity_type_filters, max_findings, strict)?;
    Ok(())
}

pub fn affected_mem_leaks(
    root_path: &Path,
    base_ref: &str,
    transitive: bool,
    project_filter: Option<&str>,
    entity_type_filters: &[String],
    max_findings: usize,
    strict: bool,
    fail_on_findings: bool,
) -> Result<()> {
    let changed_files = get_changed_files(root_path, base_ref)?;

    if changed_files.is_empty() {
        println!("No changes found between HEAD and '{}'.", base_ref);
        return Ok(());
    }

    let result = scan_and_parse_files(root_path, false)?;
    let graph = DependencyGraph::from_entities(&result.entities);

    let changed_paths: HashSet<String> = changed_files.iter().map(|cf| cf.path.clone()).collect();

    let mut direct_affected_ids: HashSet<String> = HashSet::new();
    for entity in result.entities.values() {
        if changed_paths.contains(&entity.file_path)
            && matches_project_filter(&entity.file_path, project_filter)
        {
            direct_affected_ids.insert(entity.id.clone());
        }
    }

    let consumer_ids = graph.find_consumers(&direct_affected_ids, transitive);

    let mut affected_non_test_files: HashSet<String> = HashSet::new();
    for entity in result.entities.values() {
        let is_affected =
            direct_affected_ids.contains(&entity.id) || consumer_ids.contains(&entity.id);
        if is_affected
            && matches_project_filter(&entity.file_path, project_filter)
            && entity.file_path.ends_with(".ts")
            && !is_test_file(&entity.file_path)
        {
            affected_non_test_files.insert(entity.file_path.clone());
        }
    }

    println!(
        "Affected non-test source files scanned: {}",
        affected_non_test_files.len()
    );

    if affected_non_test_files.is_empty() {
        println!("No affected non-test TypeScript files found.");
        return Ok(());
    }

    let mut sorted_affected_files: Vec<String> = affected_non_test_files.iter().cloned().collect();
    sorted_affected_files.sort();

    println!("Showing first 10 affected files:");
    for file_path in sorted_affected_files.iter().take(10) {
        println!("{}", file_path);
    }
    if sorted_affected_files.len() > 10 {
        println!(
            "...and more ({} additional files)",
            sorted_affected_files.len() - 10
        );
    }

    let scoped_entities: HashMap<String, Entity> = result
        .entities
        .into_iter()
        .filter(|(_, entity)| affected_non_test_files.contains(&entity.file_path))
        .collect();

    let report_count =
        analyze_mem_leaks_and_print(&scoped_entities, entity_type_filters, max_findings, strict)?;

    if fail_on_findings && report_count > 0 {
        anyhow::bail!(
            "Found memory leak risks in {} entities (failing due to --fail-on-findings)",
            report_count
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parser::{Parser, strip_comments, worker_filename_to_entity_name};
    use std::path::Path;

    #[test]
    fn test_extract_single_named_import() {
        let content = r#"import { Foo } from './foo';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/bar.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Foo");
        assert!(imports[0].path.contains("foo"));
    }

    #[test]
    fn test_extract_multiple_named_imports() {
        let content = r#"import { Foo, Bar, Baz } from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_extract_default_import() {
        let content = r#"import Foo from './foo';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/bar.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Foo");
    }

    #[test]
    fn test_extract_type_only_named_import() {
        let content = r#"import type { Foo, Bar } from './types';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_extract_multiline_type_only_named_import() {
        let content = r#"import type {
  Foo,
  Bar,
} from './types';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_extract_inline_type_named_import() {
        let content = r#"import { Foo, type Bar } from './types';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_extract_inline_type_named_import_with_alias() {
        let content = r#"import { type Foo as F, Bar as B } from './types';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_extract_type_only_default_import() {
        let content = r#"import type Foo from './types';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Foo");
    }

    #[test]
    fn test_extract_mixed_default_and_named_import() {
        let content = r#"import getLanguage, { getFileNameDateTimeFormat } from './get-language';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "getLanguage");
        assert_eq!(imports[1].name, "getFileNameDateTimeFormat");
        assert!(imports.iter().all(|i| i.path.contains("get-language")));
    }

    #[test]
    fn test_extract_mixed_default_and_multiple_named_imports() {
        let content = r#"import Foo, { Bar, Baz } from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
        assert_eq!(imports[2].name, "Baz");
    }

    #[test]
    fn test_extract_mixed_default_and_multiline_named_imports() {
        let content = r#"import Foo, {
  Bar,
  Baz,
} from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
        assert_eq!(imports[2].name, "Baz");
    }

    #[test]
    fn test_extract_mixed_default_and_aliased_named_imports() {
        let content = r#"import Foo, { Bar as B, type Baz } from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
        assert_eq!(imports[2].name, "Baz");
    }

    #[test]
    fn test_extract_mixed_default_and_namespace_import() {
        let content = r#"import Foo, * as FooUtils from './utils';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "FooUtils");
    }

    #[test]
    fn test_mixed_import_does_not_duplicate_bindings() {
        let content = r#"import Foo, { Bar } from './utils';
import Qux from './qux';
import { Quux } from './quux';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        let mut names: Vec<&str> = imports.iter().map(|i| i.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["Bar", "Foo", "Quux", "Qux"]);
    }

    #[test]
    fn test_extract_type_only_namespace_import() {
        let content = r#"import type * as FooTypes from './types';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "FooTypes");
    }

    #[test]
    fn test_extract_awork_alias_import() {
        let content = r#"import { Model } from '@awork/models';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/apps/web/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_import_info_has_id() {
        let content = r#"import { Foo } from './foo';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/bar.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "Bar");
    }

    #[test]
    fn test_skip_multiline_commented_import() {
        let content = r#"/* import { Foo } from './foo'; */
import { Bar } from './bar';"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

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

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "AuthModule");
        assert_eq!(imports[1].name, "DashboardModule");
    }

    #[test]
    fn test_extract_lazy_import_with_different_param_names() {
        let content = r#"loadChildren: () => import('./users/users.module').then(module => module.UsersModule)"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/app-routing.module.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "UsersModule");
    }

    #[test]
    fn test_extract_await_destructured_dynamic_import() {
        let content = r#"const { createUiAnnotator } = await import('@ui-annotator')"#;
        let root_path = Path::new("/project");
        let file_path = "/project/apps/web/src/app/service.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "createUiAnnotator");
        assert!(imports[0].path.contains("libs/ui-annotator/src/lib/create-ui-annotator"));
    }

    #[test]
    fn test_extract_multiline_aliased_destructured_dynamic_import() {
        let content = r#"const {
  Foo: LocalFoo,
  Bar = fallback,
} = await import('./lazy')"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/app/service.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "Foo");
        assert_eq!(imports[1].name, "Bar");
    }

    #[test]
    fn test_is_test_file_spec_ts() {
        assert!(super::is_test_file("/path/to/foo.spec.ts"));
        assert!(super::is_test_file("foo.spec.ts"));
    }

    #[test]
    fn test_is_test_file_test_ts() {
        assert!(super::is_test_file("/path/to/foo.test.ts"));
        assert!(super::is_test_file("foo.test.ts"));
        assert!(super::is_test_file("/path/to/foo.e2e.ts"));
    }

    #[test]
    fn test_is_test_file_non_test_files() {
        assert!(!super::is_test_file("/path/to/foo.ts"));
        assert!(!super::is_test_file("/path/to/foo.spec.tsx"));
        assert!(!super::is_test_file("/path/to/foo.test.tsx"));
        assert!(!super::is_test_file("/path/to/foo.e2e.tsx"));
        assert!(!super::is_test_file("/path/to/spec.ts.bak"));
    }

    #[test]
    fn test_find_test_files_in_directories() {
        use std::collections::HashSet;
        use std::fs::{self, File};

        // Create a temp directory with test files
        let temp_dir = std::env::temp_dir().join("sting_test_find_tests");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        // Create test files
        File::create(temp_dir.join("foo.spec.ts")).unwrap();
        File::create(temp_dir.join("bar.test.ts")).unwrap();
        File::create(temp_dir.join("baz.ts")).unwrap();

        let mut dirs: HashSet<String> = HashSet::new();
        dirs.insert(temp_dir.to_string_lossy().to_string());

        let result = super::find_test_files_in_directories(&dirs);

        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.ends_with("foo.spec.ts")));
        assert!(result.iter().any(|p| p.ends_with("bar.test.ts")));
        assert!(!result.iter().any(|p| p.ends_with("baz.ts")));

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_find_test_files_in_nonexistent_directory() {
        use std::collections::HashSet;

        let mut dirs: HashSet<String> = HashSet::new();
        dirs.insert("/nonexistent/path/that/does/not/exist".to_string());

        let result = super::find_test_files_in_directories(&dirs);

        assert!(result.is_empty());
    }

    #[test]
    fn test_worker_filename_to_entity_name_kebab_case() {
        let result = worker_filename_to_entity_name("/path/to/planner-overview.worker.ts");
        assert_eq!(result, Some("PlannerOverviewWorker".to_string()));
    }

    #[test]
    fn test_worker_filename_to_entity_name_snake_case() {
        let result = worker_filename_to_entity_name("/path/to/my_worker.worker.ts");
        assert_eq!(result, Some("MyWorkerWorker".to_string()));
    }

    #[test]
    fn test_worker_filename_to_entity_name_simple() {
        let result = worker_filename_to_entity_name("/path/to/simple.worker.ts");
        assert_eq!(result, Some("SimpleWorker".to_string()));
    }

    #[test]
    fn test_worker_filename_to_entity_name_mixed_separators() {
        let result = worker_filename_to_entity_name("/path/to/my-cool_worker.worker.ts");
        assert_eq!(result, Some("MyCoolWorkerWorker".to_string()));
    }

    #[test]
    fn test_worker_filename_to_entity_name_non_worker_file() {
        let result = worker_filename_to_entity_name("/path/to/regular.ts");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_worker_import() {
        let content = r#"const worker = new Worker(new URL('../../workers/planner-overview.worker', import.meta.url));"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/components/planning.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "PlannerOverviewWorker");
        assert!(imports[0].path.contains("planner-overview.worker"));
    }

    #[test]
    fn test_extract_worker_import_with_ts_extension() {
        let content =
            r#"const worker = new Worker(new URL('./my-worker.worker.ts', import.meta.url));"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].name, "MyWorkerWorker");
    }

    #[test]
    fn test_extract_multiple_worker_imports() {
        let content = r#"
const worker1 = new Worker(new URL('./worker-a.worker', import.meta.url));
const worker2 = new Worker(new URL('./worker-b.worker', import.meta.url));
"#;
        let root_path = Path::new("/project");
        let file_path = "/project/src/index.ts";

        let parser = Parser::new(root_path);
        let imports = parser.extract_imports(content, file_path);

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].name, "WorkerAWorker");
        assert_eq!(imports[1].name, "WorkerBWorker");
    }
}
