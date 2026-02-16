use crate::parser::{
    self, Phase, PhaseNumber, PhaseSchedulability, PhaseStatus,
};
use chrono::{Datelike, NaiveTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum PhaseAction {
    PlanAndExecute,
    Execute,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PhaseOutcome {
    Verified,
    VerificationFailed,
    ExecutionFailed,
}

pub struct ClaudeResult {
    pub success: bool,
    pub cost_usd: f64,
}

#[derive(Serialize, Deserialize)]
pub struct UsageLedger {
    pub entries: Vec<UsageEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct UsageEntry {
    pub date: String,
    pub phase: String,
    pub action: String,
    pub cost_usd: f64,
}

pub struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    fn new(path: PathBuf) -> Self {
        LockGuard { path }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        fs::remove_file(&self.path).ok();
    }
}

/// Acquire a lock file for the project. Returns None if another dispatcher is running.
pub fn acquire_lock(project: &Path) -> Option<LockGuard> {
    let lock_path = project.join(".planning").join("gsd-cron.lock");

    // Check for stale lock
    if lock_path.exists() {
        if let Ok(content) = fs::read_to_string(&lock_path) {
            if let Ok(pid) = content.trim().parse::<u32>() {
                // Check if process is still running
                let status = Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .output();
                match status {
                    Ok(output) if output.status.success() => {
                        // Process still running
                        return None;
                    }
                    _ => {
                        // Stale lock — remove it
                        eprintln!("Removing stale lock (PID {} not running)", pid);
                        fs::remove_file(&lock_path).ok();
                    }
                }
            }
        }
    }

    // Write our PID
    let pid = std::process::id();
    match fs::write(&lock_path, pid.to_string()) {
        Ok(_) => Some(LockGuard::new(lock_path)),
        Err(_) => None,
    }
}

/// Parse a window string like "HH:MM-HH:MM" into (start, end) NaiveTime.
pub fn parse_window(window: &str) -> Result<(NaiveTime, NaiveTime), String> {
    let parts: Vec<&str> = window.split('-').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid window format '{}': expected HH:MM-HH:MM", window));
    }

    let start = NaiveTime::parse_from_str(parts[0], "%H:%M")
        .map_err(|e| format!("Invalid start time '{}': {}", parts[0], e))?;
    let end = NaiveTime::parse_from_str(parts[1], "%H:%M")
        .map_err(|e| format!("Invalid end time '{}': {}", parts[1], e))?;

    Ok((start, end))
}

/// Check if the current local time is within the running window.
/// Returns true if no window is specified (no restriction).
pub fn is_within_window(window: Option<&str>) -> bool {
    let window = match window {
        Some(w) => w,
        None => return true,
    };

    let (start, end) = match parse_window(window) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Warning: {}", e);
            return false;
        }
    };

    let now = chrono::Local::now().time();

    if start > end {
        // Wraps around midnight: e.g. 23:00-05:00
        now >= start || now < end
    } else {
        // Normal range: e.g. 09:00-17:00
        now >= start && now < end
    }
}

/// Read the usage ledger from `.planning/logs/usage.json`.
pub fn read_ledger(project: &Path) -> UsageLedger {
    let path = project.join(".planning").join("logs").join("usage.json");
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or(UsageLedger { entries: vec![] }),
        Err(_) => UsageLedger { entries: vec![] },
    }
}

/// Write the usage ledger to `.planning/logs/usage.json`.
pub fn write_ledger(project: &Path, ledger: &UsageLedger) {
    let logs_dir = project.join(".planning").join("logs");
    fs::create_dir_all(&logs_dir).ok();
    let path = logs_dir.join("usage.json");
    if let Ok(json) = serde_json::to_string_pretty(ledger) {
        fs::write(&path, json).ok();
    }
}

/// Append a cost entry to the usage ledger.
fn record_cost(project: &Path, phase: &str, action: &str, cost_usd: f64) {
    let mut ledger = read_ledger(project);
    ledger.entries.push(UsageEntry {
        date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        phase: phase.to_string(),
        action: action.to_string(),
        cost_usd,
    });
    write_ledger(project, &ledger);
}

/// Sum costs from the current ISO week (Monday–Sunday).
pub fn weekly_spend(ledger: &UsageLedger) -> f64 {
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let sunday = monday + chrono::Duration::days(6);

    ledger
        .entries
        .iter()
        .filter_map(|e| {
            let d = chrono::NaiveDate::parse_from_str(&e.date, "%Y-%m-%d").ok()?;
            if d >= monday && d <= sunday {
                Some(e.cost_usd)
            } else {
                None
            }
        })
        .sum()
}

/// Check if weekly budget is exhausted. Returns true if over budget.
fn is_budget_exhausted(project: &Path, budget: f64) -> bool {
    let ledger = read_ledger(project);
    let spent = weekly_spend(&ledger);
    if spent >= budget {
        eprintln!(
            "Weekly budget of ${:.2} exhausted (${:.2} spent). Skipping.",
            budget, spent
        );
        return true;
    }
    eprintln!("Weekly spend: ${:.2} / ${:.2} budget", spent, budget);
    false
}

/// Main dispatcher run loop.
pub fn run(project: &Path, max_parallel: usize, window: Option<&str>, weekly_budget: Option<f64>) {
    if !is_within_window(window) {
        eprintln!(
            "Outside running window ({}). Skipping.",
            window.unwrap_or("unknown")
        );
        return;
    }

    if let Some(budget) = weekly_budget {
        if is_budget_exhausted(project, budget) {
            return;
        }
    }

    let _lock = match acquire_lock(project) {
        Some(l) => l,
        None => {
            eprintln!("Another dispatcher is already running for this project. Exiting.");
            return;
        }
    };

    let planning_dir = project.join(".planning");
    let logs_dir = planning_dir.join("logs");
    fs::create_dir_all(&logs_dir).ok();

    loop {
        // Check budget before each batch
        if let Some(budget) = weekly_budget {
            if is_budget_exhausted(project, budget) {
                break;
            }
        }

        // Re-read ROADMAP.md and phase dirs each iteration
        let roadmap_path = planning_dir.join("ROADMAP.md");
        let roadmap_content = match fs::read_to_string(&roadmap_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error reading ROADMAP.md: {}", e);
                break;
            }
        };

        let mut phases = parser::parse_roadmap(&roadmap_content);
        if phases.is_empty() {
            eprintln!("No phases found in ROADMAP.md");
            break;
        }

        let phase_dirs = parser::discover_phase_dirs(&planning_dir);

        for phase in &mut phases {
            parser::determine_schedulability(phase, &phase_dirs);
        }

        let ready = find_ready_phases(&phases, &phase_dirs);
        if ready.is_empty() {
            eprintln!("No ready phases found. Dispatcher complete.");
            break;
        }

        // Take up to max_parallel (sorted by phase number — lower first)
        let batch: Vec<_> = ready.into_iter().take(max_parallel).collect();

        eprintln!(
            "Dispatching {} phase(s): {}",
            batch.len(),
            batch
                .iter()
                .map(|(p, a)| format!(
                    "{} ({})",
                    p.number.display(),
                    match a {
                        PhaseAction::PlanAndExecute => "plan+execute",
                        PhaseAction::Execute => "execute",
                    }
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );

        let outcomes = execute_batch(&batch, project, &logs_dir);

        let mut any_verified = false;
        for (phase, outcome) in &outcomes {
            match outcome {
                PhaseOutcome::Verified => {
                    eprintln!("Phase {}: VERIFIED", phase.number.display());
                    any_verified = true;
                }
                PhaseOutcome::VerificationFailed => {
                    eprintln!("Phase {}: verification failed", phase.number.display());
                }
                PhaseOutcome::ExecutionFailed => {
                    eprintln!("Phase {}: execution failed", phase.number.display());
                }
            }
        }

        if !any_verified {
            eprintln!("No phases verified in this batch. Stopping.");
            break;
        }

        // Loop to check if new phases became ready
    }
}

/// Find phases that are ready to execute: deps met, not verified, schedulable/needs-planning.
pub fn find_ready_phases(
    phases: &[Phase],
    phase_dirs: &HashMap<String, PathBuf>,
) -> Vec<(Phase, PhaseAction)> {
    let mut ready = Vec::new();

    for phase in phases {
        let padded = phase.number.padded();

        // Skip already complete/verified phases
        if phase.schedulability == PhaseSchedulability::AlreadyComplete {
            continue;
        }

        // Check if already verified via VERIFICATION.md
        if let Some(dir) = phase_dirs.get(&padded) {
            if parser::has_passing_verification(dir, &phase.number) {
                continue;
            }
        }

        // Must be schedulable or needs planning (has context)
        let action = match phase.schedulability {
            PhaseSchedulability::Schedulable => PhaseAction::Execute,
            PhaseSchedulability::NeedsPlanning => PhaseAction::PlanAndExecute,
            _ => continue, // NeedsHuman, NeedsDiscussion — skip
        };

        // Check dependencies
        if !is_dependency_met(&phase.number, phases, phase_dirs) {
            continue;
        }

        ready.push((phase.clone(), action));
    }

    // Sort by phase number (lower first)
    ready.sort_by(|a, b| a.0.number.partial_cmp(&b.0.number).unwrap());
    ready
}

/// Check if a phase's dependency is met.
/// - Decimal phases depend on their parent integer phase.
/// - Integer phases depend on the previous integer phase in the sorted list (handles gaps).
/// - Phase 1 (or the first integer phase) has no dependencies.
pub fn is_dependency_met(
    phase_num: &PhaseNumber,
    all_phases: &[Phase],
    phase_dirs: &HashMap<String, PathBuf>,
) -> bool {
    if phase_num.is_decimal() {
        // Decimal phase depends on parent integer
        let parent = phase_num.parent_integer();
        return is_phase_verified_or_complete(parent as f64, all_phases, phase_dirs);
    }

    // Integer phase: find the previous integer phase in sorted order
    let mut int_phases: Vec<f64> = all_phases
        .iter()
        .filter(|p| !p.number.is_decimal())
        .map(|p| p.number.0)
        .collect();
    int_phases.sort_by(|a, b| a.partial_cmp(b).unwrap());
    int_phases.dedup();

    let current = phase_num.0;
    let predecessor = int_phases.iter().filter(|&&n| n < current).last();

    match predecessor {
        None => true, // First phase, no dependency
        Some(&prev) => is_phase_verified_or_complete(prev, all_phases, phase_dirs),
    }
}

/// Check if a phase is verified (VERIFICATION.md passed) or marked Complete in ROADMAP.md.
fn is_phase_verified_or_complete(
    phase_val: f64,
    all_phases: &[Phase],
    phase_dirs: &HashMap<String, PathBuf>,
) -> bool {
    let num = PhaseNumber(phase_val);
    let padded = num.padded();

    // Check roadmap status
    if let Some(phase) = all_phases.iter().find(|p| (p.number.0 - phase_val).abs() < 0.001) {
        if phase.status == PhaseStatus::Complete {
            return true;
        }
    }

    // Check VERIFICATION.md
    if let Some(dir) = phase_dirs.get(&padded) {
        if parser::has_passing_verification(dir, &num) {
            return true;
        }
    }

    false
}

/// Execute a batch of phases in parallel using threads.
fn execute_batch(
    batch: &[(Phase, PhaseAction)],
    project: &Path,
    logs_dir: &Path,
) -> Vec<(Phase, PhaseOutcome)> {
    let results: Arc<Mutex<Vec<(Phase, PhaseOutcome)>>> = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();

    for (phase, action) in batch {
        let phase = phase.clone();
        let action = action.clone();
        let project = project.to_path_buf();
        let log_file = logs_dir.join(format!("phase-{}.log", phase.number.display()));
        let results = Arc::clone(&results);

        let handle = std::thread::spawn(move || {
            let outcome = run_phase_lifecycle(&phase, &action, &project, &log_file);
            results.lock().unwrap().push((phase, outcome));
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().ok();
    }

    Arc::try_unwrap(results).unwrap().into_inner().unwrap()
}

/// Run the full lifecycle for a single phase.
fn run_phase_lifecycle(
    phase: &Phase,
    action: &PhaseAction,
    project: &Path,
    log_file: &Path,
) -> PhaseOutcome {
    let phase_display = phase.number.display();

    match action {
        PhaseAction::PlanAndExecute => {
            log_to_file(
                log_file,
                &format!("Phase {}: Starting plan-phase", phase_display),
            );

            let prompt = format!("/gsd:plan-phase {}", phase_display);
            let result = run_claude(&prompt, project, log_file);
            record_cost(project, &phase_display, "plan", result.cost_usd);
            if !result.success {
                log_to_file(
                    log_file,
                    &format!("Phase {}: plan-phase failed", phase_display),
                );
                return PhaseOutcome::ExecutionFailed;
            }
        }
        PhaseAction::Execute => {
            log_to_file(
                log_file,
                &format!("Phase {}: Starting execute-phase", phase_display),
            );

            let prompt = format!("/gsd:execute-phase {}", phase_display);
            let result = run_claude(&prompt, project, log_file);
            record_cost(project, &phase_display, "execute", result.cost_usd);
            if !result.success {
                log_to_file(
                    log_file,
                    &format!("Phase {}: execute-phase failed", phase_display),
                );
                return PhaseOutcome::ExecutionFailed;
            }
        }
    }

    // Run verification
    log_to_file(
        log_file,
        &format!("Phase {}: Running verification", phase_display),
    );

    let verify_prompt = format!("/gsd:verify-work {}", phase_display);
    let verify_result = run_claude(&verify_prompt, project, log_file);
    record_cost(project, &phase_display, "verify", verify_result.cost_usd);
    if !verify_result.success {
        log_to_file(
            log_file,
            &format!("Phase {}: verification command failed", phase_display),
        );
        return PhaseOutcome::VerificationFailed;
    }

    // Check if verification actually passed by reading the file
    let planning_dir = project.join(".planning");
    let phase_dirs = parser::discover_phase_dirs(&planning_dir);
    let padded = phase.number.padded();

    if let Some(dir) = phase_dirs.get(&padded) {
        if parser::has_passing_verification(dir, &phase.number) {
            log_to_file(
                log_file,
                &format!("Phase {}: VERIFIED (passed)", phase_display),
            );
            return PhaseOutcome::Verified;
        }
    }

    log_to_file(
        log_file,
        &format!("Phase {}: verification did not pass", phase_display),
    );
    PhaseOutcome::VerificationFailed
}

/// Parse `total_cost_usd` from Claude's JSON output.
/// Looks for a line containing `{"type":"result",...}` and extracts the cost.
fn parse_cost_from_output(stdout: &str) -> f64 {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if val.get("type").and_then(|t| t.as_str()) == Some("result") {
                if let Some(cost) = val.get("total_cost_usd").and_then(|c| c.as_f64()) {
                    return cost;
                }
            }
        }
    }
    0.0
}

/// Run claude CLI with the given prompt and project, appending output to log file.
/// Returns a ClaudeResult with success status and cost extracted from JSON output.
fn run_claude(prompt: &str, project: &Path, log_file: &Path) -> ClaudeResult {
    let project_str = project.display().to_string();

    log_to_file(
        log_file,
        &format!(
            "Running: claude --dangerously-skip-permissions --output-format json -p '{}' {}",
            prompt, project_str
        ),
    );

    let result = Command::new("claude")
        .args([
            "--dangerously-skip-permissions",
            "--output-format",
            "json",
            "-p",
            prompt,
            &project_str,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();

    match result {
        Ok(output) => {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let cost_usd = parse_cost_from_output(&stdout_str);

            // Append stdout and stderr to log file
            if let Ok(mut file) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file)
            {
                file.write_all(&output.stdout).ok();
                file.write_all(&output.stderr).ok();
            }
            ClaudeResult {
                success: output.status.success(),
                cost_usd,
            }
        }
        Err(e) => {
            log_to_file(log_file, &format!("Failed to run claude: {}", e));
            ClaudeResult {
                success: false,
                cost_usd: 0.0,
            }
        }
    }
}

fn log_to_file(log_file: &Path, message: &str) {
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
    {
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        writeln!(file, "[{}] {}", timestamp, message).ok();
    }
}

/// Determine the dynamic readiness label for a phase (used by status command).
pub fn readiness_label(
    phase: &Phase,
    all_phases: &[Phase],
    phase_dirs: &HashMap<String, PathBuf>,
) -> &'static str {
    let padded = phase.number.padded();

    // Check verified
    if let Some(dir) = phase_dirs.get(&padded) {
        if parser::has_passing_verification(dir, &phase.number) {
            return "VERIFIED";
        }
    }

    if phase.schedulability == PhaseSchedulability::AlreadyComplete {
        return "VERIFIED";
    }

    if phase.schedulability == PhaseSchedulability::NeedsHuman {
        return "NEEDS HUMAN";
    }

    if phase.schedulability == PhaseSchedulability::NeedsDiscussionOrPlanning {
        return "NEEDS DISCUSSION";
    }

    // Check if dependencies are met
    if !is_dependency_met(&phase.number, all_phases, phase_dirs) {
        return "BLOCKED";
    }

    match phase.schedulability {
        PhaseSchedulability::Schedulable | PhaseSchedulability::NeedsPlanning => "READY",
        _ => "BLOCKED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{PhaseNumber, PhaseSchedulability, PhaseStatus};
    use chrono::NaiveTime;

    fn make_phase(num: f64, name: &str, status: PhaseStatus, sched: PhaseSchedulability) -> Phase {
        Phase {
            number: PhaseNumber(num),
            name: name.to_string(),
            plans_complete: (0, 1),
            status,
            completed_date: None,
            schedulability: sched,
            dir_path: None,
        }
    }

    #[test]
    fn test_find_ready_phases_first_phase_ready() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        let ready = find_ready_phases(&phases, &phase_dirs);
        // Phase 1 has no deps, should be ready
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0.number.display(), "1");
        assert_eq!(ready[0].1, PhaseAction::Execute);
    }

    #[test]
    fn test_find_ready_phases_complete_predecessor() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(3.0, "API", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        let ready = find_ready_phases(&phases, &phase_dirs);
        // Phase 2 dep (phase 1) is Complete, so phase 2 is ready
        // Phase 3 dep (phase 2) is not complete, so blocked
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0.number.display(), "2");
    }

    #[test]
    fn test_find_ready_phases_needs_planning() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::NeedsPlanning),
        ];
        let phase_dirs = HashMap::new();

        let ready = find_ready_phases(&phases, &phase_dirs);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].1, PhaseAction::PlanAndExecute);
    }

    #[test]
    fn test_find_ready_phases_skips_needs_human() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
            make_phase(2.0, "Manual", PhaseStatus::NotStarted, PhaseSchedulability::NeedsHuman),
        ];
        let phase_dirs = HashMap::new();

        let ready = find_ready_phases(&phases, &phase_dirs);
        assert_eq!(ready.len(), 0);
    }

    #[test]
    fn test_is_dependency_met_first_phase() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        assert!(is_dependency_met(&PhaseNumber(1.0), &phases, &phase_dirs));
    }

    #[test]
    fn test_is_dependency_met_predecessor_complete() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        assert!(is_dependency_met(&PhaseNumber(2.0), &phases, &phase_dirs));
    }

    #[test]
    fn test_is_dependency_met_predecessor_not_complete() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        assert!(!is_dependency_met(&PhaseNumber(2.0), &phases, &phase_dirs));
    }

    #[test]
    fn test_is_dependency_met_gap_in_phases() {
        // Phase 3 depends on phase 1 (phase 2 doesn't exist)
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
            make_phase(3.0, "API", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        assert!(is_dependency_met(&PhaseNumber(3.0), &phases, &phase_dirs));
    }

    #[test]
    fn test_is_dependency_met_decimal_phase() {
        let phases = vec![
            make_phase(2.0, "Auth", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
            make_phase(2.1, "Hotfix", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        assert!(is_dependency_met(&PhaseNumber(2.1), &phases, &phase_dirs));
    }

    #[test]
    fn test_is_dependency_met_decimal_parent_not_complete() {
        let phases = vec![
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(2.1, "Hotfix", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        assert!(!is_dependency_met(&PhaseNumber(2.1), &phases, &phase_dirs));
    }

    #[test]
    fn test_readiness_label_complete() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
        ];
        let phase_dirs = HashMap::new();

        assert_eq!(readiness_label(&phases[0], &phases, &phase_dirs), "VERIFIED");
    }

    #[test]
    fn test_readiness_label_blocked() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        assert_eq!(readiness_label(&phases[1], &phases, &phase_dirs), "BLOCKED");
    }

    #[test]
    fn test_readiness_label_ready() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];
        let phase_dirs = HashMap::new();

        assert_eq!(readiness_label(&phases[1], &phases, &phase_dirs), "READY");
    }

    #[test]
    fn test_readiness_label_needs_human() {
        let phases = vec![
            make_phase(1.0, "Manual", PhaseStatus::NotStarted, PhaseSchedulability::NeedsHuman),
        ];
        let phase_dirs = HashMap::new();

        assert_eq!(readiness_label(&phases[0], &phases, &phase_dirs), "NEEDS HUMAN");
    }

    #[test]
    fn test_readiness_label_needs_discussion() {
        let phases = vec![
            make_phase(1.0, "TBD", PhaseStatus::NotStarted, PhaseSchedulability::NeedsDiscussionOrPlanning),
        ];
        let phase_dirs = HashMap::new();

        assert_eq!(readiness_label(&phases[0], &phases, &phase_dirs), "NEEDS DISCUSSION");
    }

    // --- Window tests ---

    #[test]
    fn test_parse_window_valid() {
        let (start, end) = parse_window("23:00-05:00").unwrap();
        assert_eq!(start, NaiveTime::from_hms_opt(23, 0, 0).unwrap());
        assert_eq!(end, NaiveTime::from_hms_opt(5, 0, 0).unwrap());
    }

    #[test]
    fn test_parse_window_normal_range() {
        let (start, end) = parse_window("09:00-17:00").unwrap();
        assert_eq!(start, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert_eq!(end, NaiveTime::from_hms_opt(17, 0, 0).unwrap());
    }

    #[test]
    fn test_parse_window_invalid_format() {
        assert!(parse_window("invalid").is_err());
        assert!(parse_window("23:00").is_err());
        assert!(parse_window("25:00-05:00").is_err());
        assert!(parse_window("23:00-99:00").is_err());
    }

    #[test]
    fn test_is_within_window_none() {
        // No window means always within
        assert!(is_within_window(None));
    }

    #[test]
    fn test_is_within_window_invalid() {
        // Invalid format returns false
        assert!(!is_within_window(Some("garbage")));
    }

    // Helper to test window logic with a specific time rather than relying on Local::now()
    fn time_in_window(time: NaiveTime, window: &str) -> bool {
        let (start, end) = parse_window(window).unwrap();
        if start > end {
            time >= start || time < end
        } else {
            time >= start && time < end
        }
    }

    #[test]
    fn test_window_wrap_midnight_inside_late() {
        // 23:30 is inside 23:00-05:00
        let t = NaiveTime::from_hms_opt(23, 30, 0).unwrap();
        assert!(time_in_window(t, "23:00-05:00"));
    }

    #[test]
    fn test_window_wrap_midnight_inside_early() {
        // 01:00 is inside 23:00-05:00
        let t = NaiveTime::from_hms_opt(1, 0, 0).unwrap();
        assert!(time_in_window(t, "23:00-05:00"));
    }

    #[test]
    fn test_window_wrap_midnight_outside() {
        // 12:00 is outside 23:00-05:00
        let t = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        assert!(!time_in_window(t, "23:00-05:00"));
    }

    #[test]
    fn test_window_normal_inside() {
        // 12:00 is inside 09:00-17:00
        let t = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        assert!(time_in_window(t, "09:00-17:00"));
    }

    #[test]
    fn test_window_normal_outside() {
        // 20:00 is outside 09:00-17:00
        let t = NaiveTime::from_hms_opt(20, 0, 0).unwrap();
        assert!(!time_in_window(t, "09:00-17:00"));
    }

    #[test]
    fn test_window_boundary_start_inclusive() {
        // 23:00 exactly is inside 23:00-05:00 (start is inclusive)
        let t = NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        assert!(time_in_window(t, "23:00-05:00"));
    }

    #[test]
    fn test_window_boundary_end_exclusive() {
        // 05:00 exactly is outside 23:00-05:00 (end is exclusive)
        let t = NaiveTime::from_hms_opt(5, 0, 0).unwrap();
        assert!(!time_in_window(t, "23:00-05:00"));
    }

    // --- Cost parsing tests ---

    #[test]
    fn test_parse_cost_from_output_valid() {
        let output = r#"{"type":"result","subtype":"success","total_cost_usd":0.42,"session_id":"abc123"}"#;
        assert!((parse_cost_from_output(output) - 0.42).abs() < 0.001);
    }

    #[test]
    fn test_parse_cost_from_output_no_result() {
        let output = "some random text\nno json here\n";
        assert!(parse_cost_from_output(output).abs() < 0.001);
    }

    #[test]
    fn test_parse_cost_from_output_mixed_lines() {
        let output = r#"some log output
{"type":"assistant","message":"hello"}
{"type":"result","subtype":"success","total_cost_usd":1.23,"session_id":"xyz"}"#;
        assert!((parse_cost_from_output(output) - 1.23).abs() < 0.001);
    }

    #[test]
    fn test_parse_cost_from_output_no_cost_field() {
        let output = r#"{"type":"result","subtype":"success","session_id":"abc"}"#;
        assert!(parse_cost_from_output(output).abs() < 0.001);
    }

    // --- Ledger / budget tests ---

    #[test]
    fn test_weekly_spend_current_week() {
        let today = chrono::Local::now().date_naive();
        let today_str = today.format("%Y-%m-%d").to_string();
        let ledger = UsageLedger {
            entries: vec![
                UsageEntry { date: today_str.clone(), phase: "1".into(), action: "plan".into(), cost_usd: 0.15 },
                UsageEntry { date: today_str, phase: "1".into(), action: "execute".into(), cost_usd: 0.30 },
            ],
        };
        assert!((weekly_spend(&ledger) - 0.45).abs() < 0.001);
    }

    #[test]
    fn test_weekly_spend_excludes_old_entries() {
        let old_date = (chrono::Local::now().date_naive() - chrono::Duration::days(30))
            .format("%Y-%m-%d").to_string();
        let today_str = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
        let ledger = UsageLedger {
            entries: vec![
                UsageEntry { date: old_date, phase: "1".into(), action: "plan".into(), cost_usd: 10.00 },
                UsageEntry { date: today_str, phase: "2".into(), action: "execute".into(), cost_usd: 0.50 },
            ],
        };
        assert!((weekly_spend(&ledger) - 0.50).abs() < 0.001);
    }

    #[test]
    fn test_weekly_spend_empty_ledger() {
        let ledger = UsageLedger { entries: vec![] };
        assert!(weekly_spend(&ledger).abs() < 0.001);
    }

    #[test]
    fn test_ledger_roundtrip() {
        let dir = std::env::temp_dir().join("gsd-cron-test-ledger");
        let project = dir.clone();
        fs::create_dir_all(project.join(".planning").join("logs")).ok();

        let ledger = UsageLedger {
            entries: vec![UsageEntry {
                date: "2026-02-16".into(), phase: "1".into(), action: "plan".into(), cost_usd: 0.25,
            }],
        };

        write_ledger(&project, &ledger);
        let loaded = read_ledger(&project);
        assert_eq!(loaded.entries.len(), 1);
        assert!((loaded.entries[0].cost_usd - 0.25).abs() < 0.001);

        fs::remove_dir_all(&dir).ok();
    }
}
