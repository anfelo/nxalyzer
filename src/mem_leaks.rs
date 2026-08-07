use std::collections::{HashMap, HashSet};
use std::fs;

use anyhow::Result;
use regex::Regex;

use crate::entity::{Entity, EntityType};
use crate::parser::strip_comments;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Severity {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Low => write!(f, "low"),
            Severity::Medium => write!(f, "medium"),
            Severity::High => write!(f, "high"),
        }
    }
}

#[derive(Clone, Debug)]
struct LeakFinding {
    severity: Severity,
    line: usize,
    kind: &'static str,
    message: String,
}

#[derive(Debug)]
struct EntityLeakReport {
    name: String,
    entity_type: String,
    file_path: String,
    findings: Vec<LeakFinding>,
}

#[derive(Clone, Debug)]
struct AutoUnsubscribeConfig {
    primary_array: String,
    secondary_array: String,
    include_arrays: bool,
    blacklist: HashSet<String>,
}

impl Default for AutoUnsubscribeConfig {
    fn default() -> Self {
        Self {
            primary_array: "subscriptions".to_string(),
            secondary_array: "dataSubscriptions".to_string(),
            include_arrays: false,
            blacklist: HashSet::new(),
        }
    }
}

impl AutoUnsubscribeConfig {
    fn handles_property(&self, property: &str) -> bool {
        !self.blacklist.contains(property)
    }

    fn handles_array(&self, array_name: &str) -> bool {
        !self.blacklist.contains(array_name)
            && (self.include_arrays
                || array_name == self.primary_array
                || array_name == self.secondary_array)
    }
}

pub(crate) fn analyze_and_print(
    entities: &HashMap<String, Entity>,
    entity_type_filters: &[String],
    max_findings: usize,
    strict: bool,
) -> Result<usize> {
    let mut entities_by_file: HashMap<String, Vec<&Entity>> = HashMap::new();

    for entity in entities.values() {
        if !entity_type_filters.is_empty()
            && !entity_type_filters.contains(&entity.entity_type.to_string())
        {
            continue;
        }
        entities_by_file
            .entry(entity.file_path.clone())
            .or_default()
            .push(entity);
    }

    let mut reports: Vec<EntityLeakReport> = Vec::new();

    for (file_path, file_entities) in entities_by_file {
        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Warning: Could not read file {}: {}", file_path, e);
                continue;
            }
        };

        let content = strip_comments(&content);
        let mut findings_by_entity = analyze_file_by_entity(&content, &file_entities, strict);

        for entity in file_entities {
            let mut findings = findings_by_entity.remove(&entity.id).unwrap_or_default();
            if findings.is_empty() {
                continue;
            }
            findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.line.cmp(&b.line)));

            reports.push(EntityLeakReport {
                name: entity.name.clone(),
                entity_type: entity.entity_type.to_string(),
                file_path: entity.file_path.clone(),
                findings,
            });
        }
    }

    if reports.is_empty() {
        println!("No potential memory leaks detected.");
        return Ok(0);
    }

    reports.sort_by(|a, b| {
        b.findings
            .len()
            .cmp(&a.findings.len())
            .then(top_severity(&b.findings).cmp(&top_severity(&a.findings)))
            .then(a.name.cmp(&b.name))
    });

    let report_count = reports.len();

    println!(
        "Found potential memory leak risks in {} entities:\n",
        report_count
    );
    for report in reports {
        let top = top_severity(&report.findings);
        println!(
            "{}\t{}\t{}\t{}\t{}",
            report.findings.len(),
            top,
            report.name,
            report.entity_type,
            report.file_path
        );

        for finding in report.findings.iter().take(max_findings) {
            println!(
                "  - [{}] line {}: {} ({})",
                finding.severity, finding.line, finding.message, finding.kind
            );
        }

        if report.findings.len() > max_findings {
            println!(
                "  - ... {} more findings",
                report.findings.len() - max_findings
            );
        }
    }

    Ok(report_count)
}

fn top_severity(findings: &[LeakFinding]) -> Severity {
    findings
        .iter()
        .map(|f| f.severity)
        .max()
        .unwrap_or(Severity::Low)
}

fn analyze_file_by_entity(
    content: &str,
    file_entities: &[&Entity],
    strict: bool,
) -> HashMap<String, Vec<LeakFinding>> {
    let mut result: HashMap<String, Vec<LeakFinding>> = HashMap::new();
    let total_lines = content.lines().count().max(1);

    let mut declarations: Vec<(usize, &Entity)> = file_entities
        .iter()
        .filter_map(|entity| find_entity_decl_line(content, entity).map(|line| (line, *entity)))
        .collect();

    declarations.sort_by_key(|(line, _)| *line);

    if declarations.is_empty() {
        if file_entities.len() == 1 {
            let auto_unsubscribe =
                extract_auto_unsubscribe_config(content, 1, &file_entities[0].entity_type);
            let findings = detect_segment_leaks(content, 1, true, auto_unsubscribe.as_ref(), strict);
            result.insert(file_entities[0].id.clone(), findings);
        }
        return result;
    }

    let lines: Vec<&str> = content.lines().collect();

    for (idx, (start_line, entity)) in declarations.iter().enumerate() {
        let end_line = declarations
            .get(idx + 1)
            .map(|(line, _)| line.saturating_sub(1))
            .unwrap_or(total_lines);

        if *start_line == 0 || *start_line > end_line || *start_line > lines.len() {
            continue;
        }

        let start_idx = start_line - 1;
        let end_idx = end_line.min(lines.len());
        let segment = lines[start_idx..end_idx].join("\n");
        let auto_unsubscribe =
            extract_auto_unsubscribe_config(content, *start_line, &entity.entity_type);
        let findings =
            detect_segment_leaks(&segment, *start_line, false, auto_unsubscribe.as_ref(), strict);
        if !findings.is_empty() {
            result.insert(entity.id.clone(), findings);
        }
    }

    result
}

fn find_entity_decl_line(content: &str, entity: &Entity) -> Option<usize> {
    if matches!(entity.entity_type, EntityType::Worker) {
        return Some(1);
    }

    let escaped = regex::escape(&entity.name);
    let pattern = match entity.entity_type {
        EntityType::Class
        | EntityType::Component
        | EntityType::Service
        | EntityType::Directive
        | EntityType::Pipe => {
            format!(r"(?m)^\s*export\s+(?:abstract\s+)?class\s+{}\b", escaped)
        }
        EntityType::Enum => format!(r"(?m)^\s*export\s+enum\s+{}\b", escaped),
        EntityType::Type => format!(r"(?m)^\s*export\s+type\s+{}\b", escaped),
        EntityType::Interface => format!(r"(?m)^\s*export\s+interface\s+{}\b", escaped),
        EntityType::Function => format!(
            r"(?m)^\s*export\s+function\s+{}\b|^\s*export\s+(?:const|let|var)\s+{}\b",
            escaped, escaped
        ),
        EntityType::Const => format!(r"(?m)^\s*export\s+(?:const|let|var)\s+{}\b", escaped),
        EntityType::Worker => return Some(1),
        EntityType::Unknown => return None,
    };

    let re = Regex::new(&pattern).ok()?;
    let m = re.find(content)?;
    Some(line_number_at(content, m.start()))
}

fn detect_segment_leaks(
    segment: &str,
    base_line: usize,
    uncertain_assignment: bool,
    auto_unsubscribe: Option<&AutoUnsubscribeConfig>,
    strict: bool,
) -> Vec<LeakFinding> {
    let mut findings = Vec::new();

    findings.extend(detect_rxjs_subscriptions(
        segment,
        base_line,
        uncertain_assignment,
        auto_unsubscribe,
        strict,
    ));
    findings.extend(detect_dom_listeners(
        segment,
        base_line,
        uncertain_assignment,
    ));
    findings.extend(detect_timers(segment, base_line, uncertain_assignment, strict));
    findings.extend(detect_observers(segment, base_line, uncertain_assignment));
    findings.extend(detect_streams(segment, base_line, uncertain_assignment));

    findings
}

fn detect_rxjs_subscriptions(
    segment: &str,
    base_line: usize,
    uncertain_assignment: bool,
    auto_unsubscribe: Option<&AutoUnsubscribeConfig>,
    strict: bool,
) -> Vec<LeakFinding> {
    let subscribe_re = Regex::new(r"\.subscribe\s*\(").expect("valid subscribe regex");
    let has_cleanup = Regex::new(r"\.unsubscribe\s*\(")
        .expect("valid unsubscribe regex")
        .is_match(segment)
        || Regex::new(r"takeUntilDestroyed\s*\(")
            .expect("valid takeUntilDestroyed regex")
            .is_match(segment);

    let mut out = Vec::new();
    if has_cleanup {
        return out;
    }

    let handled_by_auto = auto_unsubscribe
        .map(|cfg| collect_auto_managed_subscription_ranges(segment, cfg))
        .unwrap_or_default();

    for m in subscribe_re.find_iter(segment) {
        if is_api_subscription(segment, m.start(), m.end())
            || is_assigned_api_observable_subscription(segment, m.start(), m.end())
            || is_clipboard_subscription(segment, m.start(), m.end())
            || is_angular_to_observable_with_injector_subscription(segment, m.start(), m.end())
            || is_promise_from_subscription(segment, m.start(), m.end())
            || is_fork_join_subscription(segment, m.start(), m.end())
            || is_modal_render_subscription(segment, m.start(), m.end())
            || is_modal_service_result_subscription(segment, m.start(), m.end())
            || is_submit_action_subscription(segment, m.start(), m.end())
            || is_modal_or_popup_output_subscription(segment, m.start(), m.end())
            || is_output_to_observable_subscription(segment, m.start(), m.end())
            || (!strict && is_default_safe_finite_subscription(segment, m.start(), m.end()))
        {
            continue;
        }

        if handled_by_auto
            .iter()
            .any(|(start, end)| m.start() >= *start && m.end() <= *end)
            || auto_unsubscribe.is_some_and(|cfg| {
                is_auto_unsubscribe_array_push_subscription(segment, m.start(), m.end(), cfg)
            })
        {
            continue;
        }

        let line = base_line + line_number_at(segment, m.start()) - 1;
        let message = if auto_unsubscribe.is_some() {
            "Subscription appears without proven cleanup; @AutoUnsubscribe is present but this subscription is not clearly tracked.".to_string()
        } else {
            "Subscription appears without cleanup (unsubscribe/takeUntilDestroyed).".to_string()
        };

        out.push(LeakFinding {
            severity: adjusted_severity(Severity::High, uncertain_assignment),
            line,
            kind: "rxjs-subscription",
            message,
        });
    }

    out
}

fn is_angular_to_observable_with_injector_subscription(
    segment: &str,
    subscribe_start: usize,
    subscribe_end: usize,
) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);

    let to_observable_re = Regex::new(r"\btoObservable\s*\(").expect("valid toObservable regex");
    let injector_option_re = Regex::new(r"\{[^}]*\binjector\b[^}]*\}")
        .expect("valid toObservable injector option regex");

    to_observable_re
        .find(statement)
        .is_some_and(|m| m.start() < subscribe_start)
        && injector_option_re.is_match(statement)
}

fn is_assigned_api_observable_subscription(
    segment: &str,
    subscribe_start: usize,
    subscribe_end: usize,
) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);
    let var_re = Regex::new(r"\b([A-Za-z_$][A-Za-z0-9_$]*)\s*\.\s*subscribe\s*\(")
        .expect("valid variable subscribe regex");
    let Some(cap) = var_re.captures(statement) else {
        return false;
    };
    let var_name = &cap[1];

    let assignment_pattern = format!(
        r"(?s)\b{}\s*=\s*([^;]+Service[A-Za-z0-9_$]*\s*\.\s*([A-Za-z_$][A-Za-z0-9_$]*)(?:\s*<[^>]+>)?\s*\([^;]*)",
        regex::escape(var_name)
    );
    let Ok(assignment_re) = Regex::new(&assignment_pattern) else {
        return false;
    };

    assignment_re.captures_iter(segment).any(|assignment_cap| {
        let Some(assignment_match) = assignment_cap.get(0) else {
            return false;
        };
        assignment_match.start() < subscribe_start && api_like_method_name(&assignment_cap[2])
    })
}

fn is_clipboard_subscription(segment: &str, subscribe_start: usize, subscribe_end: usize) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);

    let clipboard_re = Regex::new(r"\bcopyToClipboard\s*\(").expect("valid clipboard regex");
    clipboard_re
        .find(statement)
        .is_some_and(|m| m.start() < subscribe_start)
}

fn is_fork_join_subscription(segment: &str, subscribe_start: usize, subscribe_end: usize) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);

    if Regex::new(r"\bforkJoin\s*\(")
        .expect("valid forkJoin regex")
        .find(statement)
        .is_some_and(|m| m.start() < subscribe_start)
    {
        return true;
    }

    let method_call_re = Regex::new(r"this\.([A-Za-z_$][A-Za-z0-9_$]*)\s*\(")
        .expect("valid this method call regex");
    for cap in method_call_re.captures_iter(statement) {
        let method_name = &cap[1];
        let method_decl_pattern = format!(
            r"(?s){}\s*\([^)]*\)\s*:[^{{]+\{{.*?\bforkJoin\s*\(",
            regex::escape(method_name)
        );
        let Ok(method_decl_re) = Regex::new(&method_decl_pattern) else {
            continue;
        };
        if method_decl_re.is_match(segment) {
            return true;
        }
    }

    false
}

fn is_promise_from_subscription(segment: &str, subscribe_start: usize, subscribe_end: usize) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);

    let from_re = Regex::new(r"\bfrom\s*\(").expect("valid from regex");
    from_re
        .find(statement)
        .is_some_and(|m| m.start() < subscribe_start)
}

fn is_api_subscription(segment: &str, subscribe_start: usize, subscribe_end: usize) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);


    let service_call_re =
        Regex::new(r"(?:[A-Za-z_$][A-Za-z0-9_$]*\s*\.\s*)*[A-Za-z_$][A-Za-z0-9_$]*Service[A-Za-z0-9_$]*\s*\.\s*([A-Za-z_$][A-Za-z0-9_$]*)(?:\s*<[^>]+>)?\s*\(")
            .expect("valid service call regex");
    for cap in service_call_re.captures_iter(statement) {
        if api_like_method_name(&cap[1]) {
            return true;
        }
    }

    let service_factory_call_re = Regex::new(
        r"(?:[A-Za-z_$][A-Za-z0-9_$]*\s*\.\s*)*[A-Za-z_$][A-Za-z0-9_$]*Service\s*\(\s*\)\s*\.\s*([A-Za-z_$][A-Za-z0-9_$]*)(?:\s*<[^>]+>)?\s*\(",
    )
    .expect("valid service factory call regex");
    for cap in service_factory_call_re.captures_iter(statement) {
        if api_like_method_name(&cap[1]) {
            return true;
        }
    }

    let api_client_re = Regex::new(
        r"(?:this\.)?apiClient\.(?:getAll|get|post|put|patch|delete|request|upload|download)(?:\s*<[^>]+>)?\s*\(",
    )
    .expect("valid api client regex");
    if api_client_re.is_match(statement) {
        return true;
    }

    let http_re = Regex::new(r"(?:this\.)?http\.(?:get|post|put|patch|delete|request)(?:\s*<[^>]+>)?\s*\(")
        .expect("valid http regex");
    if http_re.is_match(statement) {
        return true;
    }

    let this_method_re = Regex::new(r"this\.([A-Za-z_$][A-Za-z0-9_$]*)(?:\s*<[^>]+>)?\s*\(")
        .expect("valid this method regex");
    for cap in this_method_re.captures_iter(statement) {
        if api_like_method_name(&cap[1]) {
            return true;
        }
    }

    let generic_api_obj_re = Regex::new(
        r"this\.[A-Za-z_$][A-Za-z0-9_$]*(?:Api|Client)\.([A-Za-z_$][A-Za-z0-9_$]*)\s*\(",
    )
    .expect("valid api/client object regex");
    for cap in generic_api_obj_re.captures_iter(statement) {
        if api_like_method_name(&cap[1]) {
            return true;
        }
    }

    false
}

fn is_modal_render_subscription(segment: &str, subscribe_start: usize, subscribe_end: usize) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);

    let modal_render_re = Regex::new(
        r"[A-Za-z_$][A-Za-z0-9_$]*(?:Modal|ModalComponent)\.renderModal\s*\(",
    )
    .expect("valid modal render regex");

    modal_render_re.find(statement).is_some_and(|m| m.start() < subscribe_start)
}

fn is_modal_service_result_subscription(
    segment: &str,
    subscribe_start: usize,
    subscribe_end: usize,
) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);

    let modal_service_re = Regex::new(
        r"(?:[A-Za-z_$][A-Za-z0-9_$]*\s*\.\s*)*[A-Za-z_$][A-Za-z0-9_$]*(?:Modal|Popup)Service\s*\.\s*show[A-Za-z0-9_$]*(?:\s*<[^>]+>)?\s*\(",
    )
    .expect("valid modal service regex");

    modal_service_re
        .find(statement)
        .is_some_and(|m| m.start() < subscribe_start)
}

fn is_submit_action_subscription(
    segment: &str,
    subscribe_start: usize,
    subscribe_end: usize,
) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);
    Regex::new(r"\b[A-Za-z_$][A-Za-z0-9_$]*Action\s*\(")
        .expect("valid action call regex")
        .find(statement)
        .is_some_and(|m| m.start() < subscribe_start)
}

fn is_modal_or_popup_output_subscription(
    segment: &str,
    subscribe_start: usize,
    subscribe_end: usize,
) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);

    let output_re = Regex::new(
        r"[A-Za-z_$][A-Za-z0-9_$]*(?:Modal|Popup|Component)?\.(?:[A-Za-z0-9_$]*Hide|[A-Za-z0-9_$]*Close|hiding|hidden|closed|close|saved|selected|select|resolve|delete|cancel|confirm)\s*\.subscribe\s*\(",
    )
    .expect("valid modal/popup output regex");

    output_re
        .find(statement)
        .is_some_and(|m| m.start() < subscribe_start)
}

fn is_output_to_observable_subscription(
    segment: &str,
    subscribe_start: usize,
    subscribe_end: usize,
) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);

    let output_re = Regex::new(
        r"outputToObservable\s*\(\s*[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)+\s*\)\s*\.subscribe\s*\(",
    )
    .expect("valid outputToObservable regex");

    output_re
        .find(statement)
        .is_some_and(|m| m.start() < subscribe_start)
}

fn is_default_safe_finite_subscription(
    segment: &str,
    subscribe_start: usize,
    subscribe_end: usize,
) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);
    let finite_re = Regex::new(r"\b(?:take\s*\(\s*1\s*\)|first\s*\(|last\s*\()").expect(
        "valid finite operator regex",
    );

    finite_re.find(statement).is_some_and(|m| m.start() < subscribe_start)
}

fn statement_containing(segment: &str, start: usize, end: usize) -> &str {
    let statement_start = segment[..start].rfind(';').map(|idx| idx + 1).unwrap_or(0);
    let statement_end = segment[end..]
        .find(';')
        .map(|idx| end + idx)
        .unwrap_or(segment.len());
    &segment[statement_start..statement_end]
}

fn api_like_method_name(method_name: &str) -> bool {
    let lower = method_name.to_ascii_lowercase();
    let prefix_matches = [
        "fetch",
        "get",
        "create",
        "update",
        "delete",
        "send",
        "sync",
        "set",
        "post",
        "put",
        "patch",
        "watch",
        "unwatch",
        "forgot",
        "resend",
        "rename",
        "save",
        "manage",
        "link",
        "unlink",
        "verify",
        "validate",
        "check",
        "retry",
        "trigger",
        "batch",
        "bootstrap",
        "switch",
        "download",
        "upload",
        "import",
        "export",
        "increment",
        "decrement",
        "copy",
        "duplicate",
        "apply",
        "add",
        "assign",
        "unassign",
        "remove",
        "change",
    ];
    prefix_matches
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

fn is_auto_unsubscribe_array_push_subscription(
    segment: &str,
    subscribe_start: usize,
    subscribe_end: usize,
    config: &AutoUnsubscribeConfig,
) -> bool {
    let statement = statement_containing(segment, subscribe_start, subscribe_end);
    let push_re = Regex::new(r"this\.([A-Za-z_$][A-Za-z0-9_$]*)\.push\s*\(")
        .expect("valid array push regex");

    push_re.captures_iter(statement).any(|cap| {
        let Some(m) = cap.get(0) else {
            return false;
        };
        m.start() < subscribe_start && config.handles_array(&cap[1])
    })
}

fn collect_auto_managed_subscription_ranges(
    segment: &str,
    config: &AutoUnsubscribeConfig,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();

    let this_property_assign_re =
        Regex::new(r"(?s)this\.([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*.*?\.subscribe\s*\(")
            .expect("valid this property assignment regex");

    let array_push_with_subscribe_re =
        Regex::new(r"this\.([A-Za-z_$][A-Za-z0-9_$]*)\.push\s*\(\s*[^)]*?\.subscribe\s*\(")
            .expect("valid array push subscribe regex");

    let local_assign_subscribe_re = Regex::new(
        r"(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*[^;\n]*?\.subscribe\s*\(",
    )
    .expect("valid local subscribe assignment regex");

    for cap in this_property_assign_re.captures_iter(segment) {
        let property_name = cap[1].to_string();
        let m = cap.get(0).expect("capture 0 exists");
        if config.handles_property(&property_name) {
            ranges.push((m.start(), m.end()));
        }
    }

    for cap in array_push_with_subscribe_re.captures_iter(segment) {
        let array_name = cap[1].to_string();
        let m = cap.get(0).expect("capture 0 exists");
        if config.handles_array(&array_name) {
            ranges.push((m.start(), m.end()));
        }
    }

    for cap in local_assign_subscribe_re.captures_iter(segment) {
        let local_name = cap[1].to_string();
        let local_match = cap.get(0).expect("capture 0 exists");

        let push_pattern = format!(
            r"this\.([A-Za-z_$][A-Za-z0-9_$]*)\.push\s*\(\s*[^)]*\b{}\b",
            regex::escape(&local_name)
        );

        let push_re = match Regex::new(&push_pattern) {
            Ok(re) => re,
            Err(_) => continue,
        };

        let is_managed = push_re.captures_iter(segment).any(|push_cap| {
            let array_name = push_cap[1].to_string();
            config.handles_array(&array_name)
        });

        if is_managed {
            ranges.push((local_match.start(), local_match.end()));
        }
    }

    ranges
}

fn extract_auto_unsubscribe_config(
    content: &str,
    declaration_line: usize,
    entity_type: &EntityType,
) -> Option<AutoUnsubscribeConfig> {
    if !matches!(
        entity_type,
        EntityType::Class
            | EntityType::Component
            | EntityType::Service
            | EntityType::Directive
            | EntityType::Pipe
    ) {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    if declaration_line <= 1 || declaration_line > lines.len() {
        return None;
    }

    let declaration_idx = declaration_line - 1;
    let decorator_search_start = declaration_idx.saturating_sub(25);
    let decorator_region = lines[decorator_search_start..declaration_idx].join("\n");

    let decorator_re =
        Regex::new(r"(?s)@AutoUnsubscribe\s*(?:\((.*?)\))?").expect("valid decorator regex");
    let captures = decorator_re.captures(&decorator_region)?;

    let mut config = AutoUnsubscribeConfig::default();
    let options = captures.get(1).map(|m| m.as_str()).unwrap_or("");

    let array_name_re =
        Regex::new(r#"arrayName\s*:\s*['\"]([^'\"]*)['\"]"#).expect("valid arrayName regex");
    if let Some(cap) = array_name_re.captures(options) {
        config.primary_array = cap[1].to_string();
    }

    let secondary_array_name_re = Regex::new(r#"secondaryArrayName\s*:\s*['\"]([^'\"]*)['\"]"#)
        .expect("valid secondaryArrayName regex");
    if let Some(cap) = secondary_array_name_re.captures(options) {
        config.secondary_array = cap[1].to_string();
    }

    let include_arrays_re =
        Regex::new(r"includeArrays\s*:\s*(true|false)").expect("valid includeArrays regex");
    if let Some(cap) = include_arrays_re.captures(options) {
        config.include_arrays = &cap[1] == "true";
    }

    let blacklist_re =
        Regex::new(r"(?s)blacklist\s*:\s*\[([^\]]*)\]").expect("valid blacklist regex");
    if let Some(cap) = blacklist_re.captures(options) {
        let values = cap[1].to_string();
        let item_re = Regex::new(r#"['\"]([^'\"]*)['\"]"#).expect("valid quoted item regex");
        for item_cap in item_re.captures_iter(&values) {
            config.blacklist.insert(item_cap[1].to_string());
        }
    }

    Some(config)
}

fn detect_dom_listeners(
    segment: &str,
    base_line: usize,
    uncertain_assignment: bool,
) -> Vec<LeakFinding> {
    let add_named_re = Regex::new(
        r#"addEventListener\s*\(\s*['\"]([^'\"]+)['\"]\s*,\s*(?:function\s+)?([A-Za-z_$][A-Za-z0-9_$]*)"#,
    )
    .expect("valid addEventListener regex");
    let remove_named_re = Regex::new(
        r#"removeEventListener\s*\(\s*['\"]([^'\"]+)['\"]\s*,\s*([A-Za-z_$][A-Za-z0-9_$]*)"#,
    )
    .expect("valid removeEventListener regex");
    let add_any_re = Regex::new(r"addEventListener\s*\(").expect("valid generic add regex");

    let mut removed_pairs: HashMap<(String, String), usize> = HashMap::new();
    for cap in remove_named_re.captures_iter(segment) {
        let event = cap[1].to_string();
        let handler = cap[2].to_string();
        *removed_pairs.entry((event, handler)).or_insert(0) += 1;
    }

    if segment.contains("<reference lib=\"webworker\"")
        || segment.contains("<reference lib='webworker'")
        || (Regex::new(r#"addEventListener\s*\(\s*['\"]message['\"]"#)
            .expect("valid worker message listener regex")
            .is_match(segment)
            && segment.contains("postMessage("))
    {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for cap in add_named_re.captures_iter(segment) {
        let event = cap[1].to_string();
        let handler = cap[2].to_string();
        let key = (event.clone(), handler.clone());

        if let Some(count) = removed_pairs.get_mut(&key)
            && *count > 0
        {
            *count -= 1;
            continue;
        }

        let m = cap.get(0).expect("capture 0 exists");
        if listener_removes_itself(segment, m.start(), m.end(), &event, &handler)
            || listener_removed_via_property(segment, m.start(), &event, &handler)
            || is_listener_on_local_created_element(segment, m.start(), m.end())
        {
            continue;
        }

        let line = base_line + line_number_at(segment, m.start()) - 1;
        findings.push(LeakFinding {
            severity: adjusted_severity(Severity::High, uncertain_assignment),
            line,
            kind: "dom-listener",
            message: format!(
                "addEventListener('{}', {}) has no matching removeEventListener.",
                event, handler
            ),
        });
    }

    let named_add_starts: HashSet<usize> = add_named_re
        .captures_iter(segment)
        .filter_map(|cap| cap.get(0).map(|m| m.start()))
        .collect();
    for m in add_any_re.find_iter(segment) {
        // A named listener may already have been proven safe above. Do not rely on
        // occurrence counts here: named and anonymous listeners can be interleaved.
        if named_add_starts.contains(&m.start()) {
            continue;
        }
        if is_listener_on_local_created_element(segment, m.start(), m.end()) {
            continue;
        }

        let line = base_line + line_number_at(segment, m.start()) - 1;
        findings.push(LeakFinding {
            severity: adjusted_severity(Severity::Medium, uncertain_assignment),
            line,
            kind: "dom-listener",
            message: "addEventListener with anonymous/non-identifiable handler; cleanup uncertain."
                .to_string(),
        });
    }

    findings
}

fn listener_removes_itself(
    segment: &str,
    add_start: usize,
    add_end: usize,
    event: &str,
    handler: &str,
) -> bool {
    let statement = statement_containing(segment, add_start, add_end);
    let remove_pattern = format!(
        r#"removeEventListener\s*\(\s*['\"]{}['\"]\s*,\s*{}\b"#,
        regex::escape(event),
        regex::escape(handler)
    );
    let Ok(remove_re) = Regex::new(&remove_pattern) else {
        return false;
    };

    remove_re.find(statement).is_some_and(|m| m.start() > add_end)
}

fn listener_removed_via_property(
    segment: &str,
    add_start: usize,
    event: &str,
    handler: &str,
) -> bool {
    let assignment_pattern = format!(
        r"this\.([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*{}\b",
        regex::escape(handler)
    );
    let Ok(assignment_re) = Regex::new(&assignment_pattern) else {
        return false;
    };

    assignment_re.captures_iter(segment).any(|cap| {
        let Some(assignment_match) = cap.get(0) else {
            return false;
        };
        if assignment_match.start() < add_start {
            return false;
        }

        let property = &cap[1];
        let remove_pattern = format!(
            r#"removeEventListener\s*\(\s*['\"]{}['\"]\s*,\s*this\.{}\b"#,
            regex::escape(event),
            regex::escape(property)
        );
        let Ok(remove_re) = Regex::new(&remove_pattern) else {
            return false;
        };
        remove_re.is_match(segment)
    })
}

fn is_listener_on_local_created_element(segment: &str, add_start: usize, add_end: usize) -> bool {
    let statement = statement_containing(segment, add_start, add_end);
    let add_re = Regex::new(r"([A-Za-z_$][A-Za-z0-9_$]*)\.addEventListener\s*\(")
        .expect("valid add listener target regex");
    let Some(cap) = add_re.captures(statement) else {
        return false;
    };
    let target = &cap[1];

    let create_pattern = format!(
        r"(?:const|let|var)\s+{}\s*=\s*document\.createElement\s*\(",
        regex::escape(target)
    );
    let create_re = match Regex::new(&create_pattern) {
        Ok(re) => re,
        Err(_) => return false,
    };

    create_re.find(segment).is_some_and(|m| m.start() < add_start)
}

fn detect_timers(
    segment: &str,
    base_line: usize,
    uncertain_assignment: bool,
    strict: bool,
) -> Vec<LeakFinding> {
    let set_interval_re = Regex::new(r"setInterval\s*\(").expect("valid setInterval regex");
    let clear_interval_re = Regex::new(r"clearInterval\s*\(").expect("valid clearInterval regex");
    let set_timeout_re = Regex::new(r"setTimeout\s*\(").expect("valid setTimeout regex");
    let clear_timeout_re = Regex::new(r"clearTimeout\s*\(").expect("valid clearTimeout regex");

    let interval_count = set_interval_re.find_iter(segment).count();
    let cleared_interval_count = clear_interval_re.find_iter(segment).count();
    let timeout_count = set_timeout_re.find_iter(segment).count();
    let cleared_timeout_count = clear_timeout_re.find_iter(segment).count();

    let mut findings = Vec::new();
    if interval_count > cleared_interval_count {
        for m in set_interval_re
            .find_iter(segment)
            .take(interval_count - cleared_interval_count)
        {
            let line = base_line + line_number_at(segment, m.start()) - 1;
            findings.push(LeakFinding {
                severity: adjusted_severity(Severity::High, uncertain_assignment),
                line,
                kind: "timer-interval",
                message: "setInterval appears without clearInterval.".to_string(),
            });
        }
    }

    if strict && timeout_count > cleared_timeout_count {
        for m in set_timeout_re
            .find_iter(segment)
            .take(timeout_count - cleared_timeout_count)
        {
            let line = base_line + line_number_at(segment, m.start()) - 1;
            findings.push(LeakFinding {
                severity: adjusted_severity(Severity::Medium, uncertain_assignment),
                line,
                kind: "timer-timeout",
                message: "setTimeout appears without clearTimeout (strict mode).".to_string(),
            });
        }
    }

    findings
}

fn detect_observers(
    segment: &str,
    base_line: usize,
    uncertain_assignment: bool,
) -> Vec<LeakFinding> {
    let observer_new_re = Regex::new(
        r"new\s+(MutationObserver|ResizeObserver|IntersectionObserver|PerformanceObserver)\s*\(",
    )
    .expect("valid observer constructor regex");
    let disconnect_re = Regex::new(r"\.disconnect\s*\(").expect("valid disconnect regex");

    let created_count = observer_new_re.find_iter(segment).count();
    let disconnected_count = disconnect_re.find_iter(segment).count();

    if created_count <= disconnected_count
        || Regex::new(r"(?s)return\s+function\s+unsubscribe\s*\([^)]*\)\s*\{.*?\.disconnect\s*\(")
            .expect("valid observer unsubscribe cleanup regex")
            .is_match(segment)
        || Regex::new(r"(?m)^\s*export\s+function\s+create[A-Za-z0-9_$]*Observer\b")
            .expect("valid observer factory regex")
            .is_match(segment)
    {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for cap in observer_new_re
        .captures_iter(segment)
        .take(created_count - disconnected_count)
    {
        let m = cap.get(0).expect("capture 0 exists");
        let line = base_line + line_number_at(segment, m.start()) - 1;
        let observer_type = cap[1].to_string();

        findings.push(LeakFinding {
            severity: adjusted_severity(Severity::High, uncertain_assignment),
            line,
            kind: "observer",
            message: format!("{} appears without disconnect().", observer_type),
        });
    }

    findings
}

fn detect_streams(segment: &str, base_line: usize, uncertain_assignment: bool) -> Vec<LeakFinding> {
    let ws_re = Regex::new(r"new\s+WebSocket\s*\(").expect("valid websocket regex");
    let es_re = Regex::new(r"new\s+EventSource\s*\(").expect("valid eventsource regex");
    let close_re = Regex::new(r"\.close\s*\(").expect("valid close regex");

    let mut findings = Vec::new();

    let ws_count = ws_re.find_iter(segment).count();
    let es_count = es_re.find_iter(segment).count();
    let close_count = close_re.find_iter(segment).count();

    let created_streams = ws_count + es_count;
    if created_streams <= close_count {
        return findings;
    }

    for m in ws_re.find_iter(segment) {
        let line = base_line + line_number_at(segment, m.start()) - 1;
        findings.push(LeakFinding {
            severity: adjusted_severity(Severity::High, uncertain_assignment),
            line,
            kind: "websocket",
            message: "WebSocket appears without close().".to_string(),
        });
    }
    for m in es_re.find_iter(segment) {
        let line = base_line + line_number_at(segment, m.start()) - 1;
        findings.push(LeakFinding {
            severity: adjusted_severity(Severity::High, uncertain_assignment),
            line,
            kind: "eventsource",
            message: "EventSource appears without close().".to_string(),
        });
    }

    findings
        .into_iter()
        .take(created_streams - close_count)
        .collect()
}

fn adjusted_severity(base: Severity, uncertain_assignment: bool) -> Severity {
    if !uncertain_assignment {
        return base;
    }
    match base {
        Severity::High => Severity::Medium,
        _ => base,
    }
}

fn line_number_at(content: &str, byte_idx: usize) -> usize {
    content[..byte_idx].bytes().filter(|b| *b == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::{AutoUnsubscribeConfig, detect_segment_leaks};

    #[test]
    fn flags_unmanaged_subscription() {
        let content = "this.stream.subscribe(v => v);";
        let findings = detect_segment_leaks(content, 10, false, None, false);
        assert!(findings.iter().any(|f| f.kind == "rxjs-subscription"));
    }

    #[test]
    fn skips_managed_subscription_take_until_destroyed() {
        let content = "this.stream.pipe(takeUntilDestroyed()).subscribe(v => v);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| f.kind != "rxjs-subscription"));
    }

    #[test]
    fn suppresses_subscription_with_take_one() {
        let content = "this.stream.pipe(take(1)).subscribe(v => v);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| f.kind != "rxjs-subscription"));
    }

    #[test]
    fn strict_mode_flags_subscription_with_take_one() {
        let content = "this.stream.pipe(take(1)).subscribe(v => v);";
        let findings = detect_segment_leaks(content, 1, false, None, true);
        assert!(findings.iter().any(|f| f.kind == "rxjs-subscription"));
    }

    #[test]
    fn still_flags_subscription_with_take_more_than_one() {
        let content = "this.stream.pipe(take(2)).subscribe(v => v);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().any(|f| f.kind == "rxjs-subscription"));
    }

    #[test]
    fn suppresses_subscription_with_first_operator_by_default() {
        let content = "this.stream.pipe(first()).subscribe(v => v);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| f.kind != "rxjs-subscription"));
    }

    #[test]
    fn strict_mode_flags_subscription_with_first_operator() {
        let content = "this.stream.pipe(first()).subscribe(v => v);";
        let findings = detect_segment_leaks(content, 1, false, None, true);
        assert!(findings.iter().any(|f| f.kind == "rxjs-subscription"));
    }

    #[test]
    fn suppresses_subscription_with_last_operator_by_default() {
        let content = "this.stream.pipe(last()).subscribe(v => v);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| f.kind != "rxjs-subscription"));
    }

    #[test]
    fn strict_mode_flags_subscription_with_last_operator() {
        let content = "this.stream.pipe(last()).subscribe(v => v);";
        let findings = detect_segment_leaks(content, 1, false, None, true);
        assert!(findings.iter().any(|f| f.kind == "rxjs-subscription"));
    }

    #[test]
    fn suppresses_subscription_with_auto_unsubscribe_when_assigned_to_property() {
        let content = "this.fetchSubscription = this.service.fetch().subscribe();";
        let auto = AutoUnsubscribeConfig::default();
        let findings = detect_segment_leaks(content, 1, false, Some(&auto), false);
        assert!(findings.iter().all(|f| f.kind != "rxjs-subscription"));
    }

    #[test]
    fn does_not_suppress_auto_unsubscribe_when_tracking_is_not_proven() {
        let content = "this.service.fetch().subscribe();";
        let auto = AutoUnsubscribeConfig::default();
        let findings = detect_segment_leaks(content, 1, false, Some(&auto), false);
        assert!(findings.iter().any(|f| f.kind == "rxjs-subscription"));
    }

    #[test]
    fn respects_auto_unsubscribe_blacklist() {
        let content = "this.fetchSubscription = this.service.fetch().subscribe();";
        let mut auto = AutoUnsubscribeConfig::default();
        auto.blacklist.insert("fetchSubscription".to_string());
        let findings = detect_segment_leaks(content, 1, false, Some(&auto), false);
        assert!(findings.iter().any(|f| f.kind == "rxjs-subscription"));
    }

    #[test]
    fn suppresses_named_dom_listener_stored_for_later_cleanup() {
        let content = r#"
            const listener = (event: MouseEvent): void => event.preventDefault();
            element.addEventListener('mousedown', listener);
            this.listener = listener;
            this.elementWithListener = element;

            if (this.elementWithListener && this.listener) {
                this.elementWithListener.removeEventListener('mousedown', this.listener);
            }
            this.listener = null;
            this.elementWithListener = null;
        "#;
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| f.kind != "dom-listener"));
    }

    #[test]
    fn does_not_reclassify_safe_named_listener_after_anonymous_listener() {
        let content = r#"
            element.addEventListener('click', () => doSomething());
            const listener = (event: MouseEvent): void => event.preventDefault();
            element.addEventListener('mousedown', listener);
            this.listener = listener;
            element.removeEventListener('mousedown', this.listener);
        "#;
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert_eq!(
            findings.iter().filter(|f| f.kind == "dom-listener").count(),
            1
        );
    }

    #[test]
    fn flags_interval_without_clear() {
        let content = "const id = setInterval(tick, 1000);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().any(|f| f.kind == "timer-interval"));
    }

    #[test]
    fn ignores_set_timeout_without_clear_by_default() {
        let content = "setTimeout(() => doStuff(), 1000);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| !f.kind.starts_with("timer-")));
    }

    #[test]
    fn strict_mode_flags_set_timeout_without_clear() {
        let content = "setTimeout(() => doStuff(), 1000);";
        let findings = detect_segment_leaks(content, 1, false, None, true);
        assert!(findings.iter().any(|f| f.kind == "timer-timeout"));
    }

    #[test]
    fn flags_observer_without_disconnect() {
        let content = "const o = new MutationObserver(() => {}); o.observe(node);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().any(|f| f.kind == "observer"));
    }

    #[test]
    fn flags_websocket_without_close() {
        let content = "const socket = new WebSocket(url);";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().any(|f| f.kind == "websocket"));
    }

    #[test]
    fn suppresses_api_service_subscription() {
        let content = "this.userService.fetchUser(userId).subscribe();";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| f.kind != "rxjs-subscription"));
    }

    #[test]
    fn suppresses_modal_render_subscription() {
        let content = "TaskDetailModalComponent.renderModal(this.notification.task).subscribe();";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| f.kind != "rxjs-subscription"));
    }

    #[test]
    fn suppresses_modal_render_output_subscription() {
        let content = "AddWorkspaceAbsenceModalComponent.renderModal().saved.subscribe(() => {});";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().all(|f| f.kind != "rxjs-subscription"));
    }

    #[test]
    fn keeps_non_api_subscription_findings() {
        let content = "this.taskQuery.selectActiveTask().subscribe();";
        let findings = detect_segment_leaks(content, 1, false, None, false);
        assert!(findings.iter().any(|f| f.kind == "rxjs-subscription"));
    }
}
