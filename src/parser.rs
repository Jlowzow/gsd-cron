use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum PhaseStatus {
    NotStarted,
    InProgress,
    Complete,
    Deferred,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PhaseSchedulability {
    Schedulable,
    NeedsHuman,
    NeedsDiscussionOrPlanning,
    NeedsPlanning,
    AlreadyComplete,
}

#[derive(Debug, Clone)]
pub struct Phase {
    pub number: PhaseNumber,
    pub name: String,
    #[allow(dead_code)]
    pub plans_complete: (u32, u32),
    pub status: PhaseStatus,
    #[allow(dead_code)]
    pub completed_date: Option<String>,
    pub schedulability: PhaseSchedulability,
    pub dir_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct PhaseNumber(pub f64);

impl PhaseNumber {
    pub fn parse(s: &str) -> Option<Self> {
        s.trim().parse::<f64>().ok().map(PhaseNumber)
    }

    pub fn is_decimal(&self) -> bool {
        self.0.fract() != 0.0
    }

    pub fn parent_integer(&self) -> u32 {
        self.0.floor() as u32
    }

    pub fn display(&self) -> String {
        if self.is_decimal() {
            // Format like "2.1"
            format!("{}", self.0)
        } else {
            format!("{}", self.0 as u32)
        }
    }

    /// Zero-padded form for directory matching (e.g., "01", "02")
    pub fn padded(&self) -> String {
        if self.is_decimal() {
            let int_part = self.0.floor() as u32;
            let frac = ((self.0 - self.0.floor()) * 10.0).round() as u32;
            format!("{:02}.{}", int_part, frac)
        } else {
            format!("{:02}", self.0 as u32)
        }
    }
}

impl std::fmt::Display for PhaseNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display())
    }
}

#[derive(Debug)]
pub struct VerificationInfo {
    pub status: String,
}

pub fn parse_roadmap(content: &str) -> Vec<Phase> {
    let mut phases = Vec::new();

    // Match the progress table rows
    // Format 1: | 1. Name | 0/3 | Not started | - |
    // Format 2: | 1. Name | v1.0 | 0/3 | Not started | - |  (with milestone)
    // Format 3: | Phase 1: Name | Status | Requirements | 100% |  (GSD v2)
    let row_re = Regex::new(
        r"(?m)^\|\s*(?:Phase\s+)?(\d+(?:\.\d+)?)[.:]\s+(.+?)\s*\|(.+)\|$"
    ).unwrap();

    for cap in row_re.captures_iter(content) {
        let phase_num_str = &cap[1];
        let name = cap[2].trim().to_string();
        let rest = &cap[3];

        let phase_number = match PhaseNumber::parse(phase_num_str) {
            Some(n) => n,
            None => continue,
        };

        // Split remaining columns by pipe
        let cols: Vec<&str> = rest.split('|').map(|s| s.trim()).collect();

        // Find plans_complete (N/M pattern) and status columns
        let mut plans_complete = (0u32, 0u32);
        let mut status = PhaseStatus::NotStarted;
        let mut completed_date = None;

        for col in &cols {
            if let Some(pc) = parse_plans_complete(col) {
                plans_complete = pc;
            } else if let Some(s) = parse_status(col) {
                status = s;
                // Also extract embedded date from status like "✓ Complete (2026-02-15)"
                if completed_date.is_none() {
                    completed_date = extract_embedded_date(col);
                }
            } else if is_date(col) {
                completed_date = Some(col.to_string());
            }
        }

        phases.push(Phase {
            number: phase_number,
            name,
            plans_complete,
            status,
            completed_date,
            schedulability: PhaseSchedulability::Schedulable, // determined later
            dir_path: None,
        });
    }

    phases
}

fn parse_plans_complete(s: &str) -> Option<(u32, u32)> {
    // Try N/M format first (e.g., "3/3", "0/2")
    let re = Regex::new(r"^(\d+)/(\d+)$").unwrap();
    if let Some(cap) = re.captures(s) {
        let done = cap[1].parse().unwrap_or(0);
        let total = cap[2].parse().unwrap_or(0);
        return Some((done, total));
    }

    // Try percentage format (e.g., "100%", "0%")
    let pct_re = Regex::new(r"^(\d+)%$").unwrap();
    if let Some(cap) = pct_re.captures(s) {
        let pct: u32 = cap[1].parse().unwrap_or(0);
        return Some((pct, 100));
    }

    None
}

fn parse_status(s: &str) -> Option<PhaseStatus> {
    let lower = s.to_lowercase();
    let trimmed = lower.trim();
    match trimmed {
        "not started" | "pending" => Some(PhaseStatus::NotStarted),
        "in progress" => Some(PhaseStatus::InProgress),
        "complete" => Some(PhaseStatus::Complete),
        "deferred" => Some(PhaseStatus::Deferred),
        _ => {
            // Handle "✓ Complete (date)" or similar patterns
            if trimmed.contains("complete") {
                return Some(PhaseStatus::Complete);
            }
            if trimmed.contains("in progress") {
                return Some(PhaseStatus::InProgress);
            }
            None
        }
    }
}

/// Extract an embedded date from a string like "✓ Complete (2026-02-15)"
fn extract_embedded_date(s: &str) -> Option<String> {
    let re = Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap();
    re.find(s).map(|m| m.as_str().to_string())
}

fn is_date(s: &str) -> bool {
    let re = Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap();
    re.is_match(s)
}

pub fn parse_verification(content: &str) -> Option<VerificationInfo> {
    // Look in YAML frontmatter for status field
    let fm_re = Regex::new(r"(?s)^---\s*\n(.*?)\n---").unwrap();
    if let Some(fm_cap) = fm_re.captures(content) {
        let frontmatter = &fm_cap[1];
        let status_re = Regex::new(r"(?m)^status:\s*(.+)$").unwrap();
        if let Some(s_cap) = status_re.captures(frontmatter) {
            return Some(VerificationInfo {
                status: s_cap[1].trim().to_string(),
            });
        }
    }
    None
}

/// Check if any plan in a phase directory has `autonomous: false`
pub fn has_non_autonomous_plan(phase_dir: &Path, phase_num: &PhaseNumber) -> bool {
    let padded = phase_num.padded();

    if let Ok(entries) = fs::read_dir(phase_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if matches_plan_pattern(&name, &padded) {
                if let Ok(content) = fs::read_to_string(entry.path()) {
                    if is_autonomous_false(&content) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn matches_plan_pattern(filename: &str, padded_phase: &str) -> bool {
    filename.starts_with(&format!("{}-", padded_phase)) && filename.ends_with("-PLAN.md")
}

fn is_autonomous_false(content: &str) -> bool {
    let fm_re = Regex::new(r"(?s)^---\s*\n(.*?)\n---").unwrap();
    if let Some(fm_cap) = fm_re.captures(content) {
        let frontmatter = &fm_cap[1];
        let auto_re = Regex::new(r"(?m)^autonomous:\s*(false|true)").unwrap();
        if let Some(a_cap) = auto_re.captures(frontmatter) {
            return &a_cap[1] == "false";
        }
    }
    false
}

/// Check if a phase has plan files
pub fn has_plan_files(phase_dir: &Path, phase_num: &PhaseNumber) -> bool {
    let padded = phase_num.padded();
    if let Ok(entries) = fs::read_dir(phase_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if matches_plan_pattern(&name, &padded) {
                return true;
            }
        }
    }
    false
}

/// Check if a phase has a CONTEXT.md file
pub fn has_context_file(phase_dir: &Path, phase_num: &PhaseNumber) -> bool {
    let padded = phase_num.padded();
    let context_name = format!("{}-CONTEXT.md", padded);
    phase_dir.join(&context_name).exists()
}

/// Check if a phase has a passing VERIFICATION.md
pub fn has_passing_verification(phase_dir: &Path, phase_num: &PhaseNumber) -> bool {
    let padded = phase_num.padded();
    let verification_name = format!("{}-VERIFICATION.md", padded);
    let path = phase_dir.join(&verification_name);
    if let Ok(content) = fs::read_to_string(&path) {
        if let Some(info) = parse_verification(&content) {
            return info.status == "passed";
        }
    }
    false
}

/// Discover phase directories and map phase numbers to their directory paths
pub fn discover_phase_dirs(planning_dir: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let phases_dir = planning_dir.join("phases");

    if let Ok(entries) = fs::read_dir(&phases_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let dir_name = entry.file_name().to_string_lossy().to_string();
                // Directory names are like "01-foundation", "02-features", "02.1-hotfix"
                if let Some(phase_prefix) = dir_name.split('-').next() {
                    map.insert(phase_prefix.to_string(), entry.path());
                }
            }
        }
    }

    map
}

/// Determine schedulability of a phase based on its directory contents
pub fn determine_schedulability(
    phase: &mut Phase,
    phase_dirs: &HashMap<String, PathBuf>,
) {
    if phase.status == PhaseStatus::Complete {
        phase.schedulability = PhaseSchedulability::AlreadyComplete;
        return;
    }

    if phase.status == PhaseStatus::Deferred {
        phase.schedulability = PhaseSchedulability::NeedsDiscussionOrPlanning;
        return;
    }

    let padded = phase.number.padded();
    let dir = match phase_dirs.get(&padded) {
        Some(d) => {
            phase.dir_path = Some(d.clone());
            d
        }
        None => {
            phase.schedulability = PhaseSchedulability::NeedsDiscussionOrPlanning;
            return;
        }
    };

    let has_plans = has_plan_files(dir, &phase.number);
    let has_context = has_context_file(dir, &phase.number);

    if has_plans {
        if has_non_autonomous_plan(dir, &phase.number) {
            phase.schedulability = PhaseSchedulability::NeedsHuman;
        } else {
            phase.schedulability = PhaseSchedulability::Schedulable;
        }
    } else if has_context {
        phase.schedulability = PhaseSchedulability::NeedsPlanning;
    } else {
        phase.schedulability = PhaseSchedulability::NeedsDiscussionOrPlanning;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_roadmap_basic() {
        let content = r#"
## Progress

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Foundation | 3/3 | Complete | 2026-01-15 |
| 2. Auth System | 1/2 | In progress | - |
| 3. API Layer | 0/3 | Not started | - |
| 4. Frontend | 0/1 | Not started | - |
"#;
        let phases = parse_roadmap(content);
        assert_eq!(phases.len(), 4);

        assert_eq!(phases[0].number.display(), "1");
        assert_eq!(phases[0].name, "Foundation");
        assert_eq!(phases[0].plans_complete, (3, 3));
        assert_eq!(phases[0].status, PhaseStatus::Complete);
        assert_eq!(phases[0].completed_date, Some("2026-01-15".to_string()));

        assert_eq!(phases[1].number.display(), "2");
        assert_eq!(phases[1].name, "Auth System");
        assert_eq!(phases[1].plans_complete, (1, 2));
        assert_eq!(phases[1].status, PhaseStatus::InProgress);

        assert_eq!(phases[2].number.display(), "3");
        assert_eq!(phases[2].status, PhaseStatus::NotStarted);
    }

    #[test]
    fn test_parse_roadmap_with_decimals() {
        let content = r#"
| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Foundation | 3/3 | Complete | 2026-01-15 |
| 2. Auth | 2/2 | Complete | 2026-01-20 |
| 2.1. Hotfix | 1/1 | Complete | 2026-01-21 |
| 2.2. Security Patch | 0/1 | Not started | - |
| 3. API | 0/2 | Not started | - |
"#;
        let phases = parse_roadmap(content);
        assert_eq!(phases.len(), 5);
        assert!(phases[2].number.is_decimal());
        assert_eq!(phases[2].number.parent_integer(), 2);
        assert!(phases[3].number.is_decimal());
    }

    #[test]
    fn test_parse_roadmap_with_milestone() {
        let content = r#"
| Phase | Milestone | Plans Complete | Status | Completed |
|-------|-----------|----------------|--------|-----------|
| 1. Foundation | v1.0 | 3/3 | Complete | 2026-01-15 |
| 2. Auth | v1.0 | 0/2 | Not started | - |
"#;
        let phases = parse_roadmap(content);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].plans_complete, (3, 3));
        assert_eq!(phases[0].status, PhaseStatus::Complete);
    }

    #[test]
    fn test_parse_roadmap_gsd_v2_format() {
        let content = r#"
## Progress

| Phase | Status | Requirements | Completion |
|-------|--------|--------------|------------|
| Phase 1: Foundation & Multi-Tenant Architecture | ✓ Complete (2026-02-15) | TENANT-01, TENANT-02 | 100% |
| Phase 2: Core Storage & Database Layer | Pending | DEPLOY-01, DEPLOY-02 | 0% |
| Phase 3: Document Ingestion Pipeline | Pending | INGEST-01, INGEST-02 | 0% |
| Phase 11: Production Hardening & Scale Testing | Pending | (Production readiness) | 0% |
"#;
        let phases = parse_roadmap(content);
        assert_eq!(phases.len(), 4, "Expected 4 phases, got {}: {:?}", phases.len(), phases.iter().map(|p| &p.name).collect::<Vec<_>>());

        // Phase 1: complete with date
        assert_eq!(phases[0].number.display(), "1");
        assert_eq!(phases[0].name, "Foundation & Multi-Tenant Architecture");
        assert_eq!(phases[0].status, PhaseStatus::Complete);
        assert_eq!(phases[0].completed_date, Some("2026-02-15".to_string()));
        assert_eq!(phases[0].plans_complete, (100, 100));

        // Phase 2: pending
        assert_eq!(phases[1].number.display(), "2");
        assert_eq!(phases[1].name, "Core Storage & Database Layer");
        assert_eq!(phases[1].status, PhaseStatus::NotStarted);
        assert_eq!(phases[1].plans_complete, (0, 100));

        // Phase 11: double-digit phase number
        assert_eq!(phases[3].number.display(), "11");
        assert_eq!(phases[3].name, "Production Hardening & Scale Testing");
    }

    #[test]
    fn test_parse_roadmap_full_ragbrain() {
        // Full 11-phase roadmap like RAGbrain produces
        let content = r#"
## Progress

| Phase | Status | Requirements | Completion |
|-------|--------|--------------|------------|
| Phase 1: Foundation & Multi-Tenant Architecture | ✓ Complete (2026-02-15) | TENANT-01, TENANT-02, TENANT-03, TENANT-04, TENANT-05, TENANT-07, TENANT-08, TENANT-09 | 100% |
| Phase 2: Core Storage & Database Layer | Pending | DEPLOY-01, DEPLOY-02, DEPLOY-03 | 0% |
| Phase 3: Document Ingestion Pipeline | Pending | INGEST-01, INGEST-02, INGEST-03, INGEST-04, INGEST-05, INGEST-06 | 0% |
| Phase 4: Directory Ingestion & Watching | Pending | INGEST-07, INGEST-08 | 0% |
| Phase 5: Hybrid Retrieval & Search | Pending | RETR-01, RETR-02, RETR-04, TENANT-06 | 0% |
| Phase 6: LLM Integration & Answer Generation | Pending | LLM-01, LLM-02, LLM-03, LLM-04, LLM-05, RETR-03 | 0% |
| Phase 7: Semantic Caching & Performance Optimization | Pending | RETR-05, RETR-06 | 0% |
| Phase 8: REST API & Authentication | Pending | API-01, API-02, API-03, API-04, API-05 | 0% |
| Phase 9: External Integrations | Pending | API-06, API-07 | 0% |
| Phase 10: Observability & Debugging | Pending | OBS-01, OBS-02, OBS-03, OBS-04 | 0% |
| Phase 11: Production Hardening & Scale Testing | Pending | (Production readiness) | 0% |
"#;
        let phases = parse_roadmap(content);
        assert_eq!(phases.len(), 11, "Expected 11 phases, got {}", phases.len());
    }

    #[test]
    fn test_parse_status_variants() {
        assert_eq!(parse_status("Pending"), Some(PhaseStatus::NotStarted));
        assert_eq!(parse_status("pending"), Some(PhaseStatus::NotStarted));
        assert_eq!(parse_status("Not started"), Some(PhaseStatus::NotStarted));
        assert_eq!(parse_status("In progress"), Some(PhaseStatus::InProgress));
        assert_eq!(parse_status("Complete"), Some(PhaseStatus::Complete));
        assert_eq!(parse_status("✓ Complete (2026-02-15)"), Some(PhaseStatus::Complete));
        assert_eq!(parse_status("Deferred"), Some(PhaseStatus::Deferred));
    }

    #[test]
    fn test_extract_embedded_date() {
        assert_eq!(extract_embedded_date("✓ Complete (2026-02-15)"), Some("2026-02-15".to_string()));
        assert_eq!(extract_embedded_date("Complete"), None);
        assert_eq!(extract_embedded_date("Pending"), None);
    }

    #[test]
    fn test_parse_plans_complete_percentage() {
        assert_eq!(parse_plans_complete("100%"), Some((100, 100)));
        assert_eq!(parse_plans_complete("0%"), Some((0, 100)));
        assert_eq!(parse_plans_complete("50%"), Some((50, 100)));
        assert_eq!(parse_plans_complete("3/3"), Some((3, 3)));
        assert_eq!(parse_plans_complete("0/2"), Some((0, 2)));
    }

    #[test]
    fn test_phase_number_ordering() {
        let p1 = PhaseNumber(1.0);
        let p1_1 = PhaseNumber(1.1);
        let p2 = PhaseNumber(2.0);
        let p2_1 = PhaseNumber(2.1);
        let p2_2 = PhaseNumber(2.2);
        let p3 = PhaseNumber(3.0);

        assert!(p1 < p1_1);
        assert!(p1_1 < p2);
        assert!(p2 < p2_1);
        assert!(p2_1 < p2_2);
        assert!(p2_2 < p3);
    }

    #[test]
    fn test_phase_number_padded() {
        assert_eq!(PhaseNumber(1.0).padded(), "01");
        assert_eq!(PhaseNumber(2.0).padded(), "02");
        assert_eq!(PhaseNumber(2.1).padded(), "02.1");
        assert_eq!(PhaseNumber(12.0).padded(), "12");
    }

    #[test]
    fn test_is_autonomous_false() {
        let content = r#"---
phase: 01-foundation
plan: 01
type: execute
wave: 1
depends_on: []
files_modified: []
autonomous: false
must_haves:
  truths:
    - "Something works"
---

# Plan content
"#;
        assert!(is_autonomous_false(content));
    }

    #[test]
    fn test_is_autonomous_true() {
        let content = r#"---
phase: 01-foundation
plan: 01
autonomous: true
---

# Plan content
"#;
        assert!(!is_autonomous_false(content));
    }

    #[test]
    fn test_parse_verification_passed() {
        let content = r#"---
phase: 01-foundation
verified: 2026-01-15T10:00:00Z
status: passed
score: 5/5 must-haves verified
---

# Verification Report
"#;
        let info = parse_verification(content).unwrap();
        assert_eq!(info.status, "passed");
    }

    #[test]
    fn test_parse_verification_gaps_found() {
        let content = r#"---
phase: 02-auth
verified: 2026-01-20T10:00:00Z
status: gaps_found
score: 3/5 must-haves verified
---
"#;
        let info = parse_verification(content).unwrap();
        assert_eq!(info.status, "gaps_found");
    }

}
