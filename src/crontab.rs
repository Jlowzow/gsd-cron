use crate::scheduler::ScheduleSlot;
use std::path::Path;
use std::process::Command;

const TAG_PREFIX: &str = "# gsd-cron:";

/// Generate crontab entry lines for a schedule.
/// Each entry runs the wrapper script with the phase number as argument.
pub fn generate_entries(
    slots: &[ScheduleSlot],
    project_path: &Path,
    wrapper_path: &Path,
) -> Vec<String> {
    let mut lines = Vec::new();
    let project_str = project_path.display().to_string();
    let wrapper_str = wrapper_path.display().to_string();

    lines.push(format!("{}{}", TAG_PREFIX, project_str));

    for slot in slots {
        let minute = slot.time.format("%M").to_string();
        let hour = slot.time.format("%H").to_string();
        // Remove leading zeros for cron compatibility
        let minute = minute.trim_start_matches('0');
        let minute = if minute.is_empty() { "0" } else { minute };
        let hour = hour.trim_start_matches('0');
        let hour = if hour.is_empty() { "0" } else { hour };

        for phase in &slot.phases {
            let phase_display = phase.number.display();
            lines.push(format!(
                "{} {} * * * {} {} # gsd-cron:{} phase {}",
                minute,
                hour,
                wrapper_str,
                phase_display,
                project_str,
                phase_display,
            ));
        }
    }

    lines.push(format!("{}{} END", TAG_PREFIX, project_str));
    lines
}

/// Read the current user crontab
pub fn read_crontab() -> Result<String, String> {
    let output = Command::new("crontab")
        .arg("-l")
        .output()
        .map_err(|e| format!("Failed to read crontab: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        // Empty crontab returns non-zero on some systems
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no crontab") {
            Ok(String::new())
        } else {
            Err(format!("Failed to read crontab: {}", stderr))
        }
    }
}

/// Write a new crontab
fn write_crontab(content: &str) -> Result<(), String> {
    use std::io::Write;

    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to write crontab: {}", e))?;

    if let Some(ref mut stdin) = child.stdin {
        stdin
            .write_all(content.as_bytes())
            .map_err(|e| format!("Failed to write to crontab stdin: {}", e))?;
    }

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait for crontab: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err("crontab command failed".to_string())
    }
}

/// Install crontab entries for a project (removes existing entries for the project first)
pub fn install(
    slots: &[ScheduleSlot],
    project_path: &Path,
    wrapper_path: &Path,
) -> Result<(), String> {
    let current = read_crontab()?;
    let cleaned = remove_project_entries(&current, project_path);
    let new_entries = generate_entries(slots, project_path, wrapper_path);

    let mut final_content = cleaned;
    if !final_content.is_empty() && !final_content.ends_with('\n') {
        final_content.push('\n');
    }
    final_content.push_str(&new_entries.join("\n"));
    final_content.push('\n');

    write_crontab(&final_content)
}

/// Remove all crontab entries for a project
pub fn remove(project_path: &Path) -> Result<(), String> {
    let current = read_crontab()?;
    let cleaned = remove_project_entries(&current, project_path);

    if cleaned.trim().is_empty() {
        // Remove crontab entirely if nothing left
        Command::new("crontab")
            .arg("-r")
            .output()
            .map_err(|e| format!("Failed to remove crontab: {}", e))?;
        Ok(())
    } else {
        write_crontab(&cleaned)
    }
}

/// Filter out lines belonging to a specific project
fn remove_project_entries(crontab_content: &str, project_path: &Path) -> String {
    let project_str = project_path.display().to_string();
    let tag = format!("{}{}", TAG_PREFIX, project_str);

    let mut result = Vec::new();
    let mut skipping = false;

    for line in crontab_content.lines() {
        if line.starts_with(&tag) {
            if line.ends_with(" END") {
                skipping = false;
                continue;
            }
            skipping = true;
            continue;
        }

        if skipping {
            // Check if this line belongs to our project (inline tag)
            if line.contains(&format!("gsd-cron:{}", project_str)) {
                continue;
            }
        }

        if !skipping {
            result.push(line);
        }
    }

    result.join("\n")
}

/// Get status of scheduled phases for a project from the current crontab
pub fn get_scheduled_phases(project_path: &Path) -> Result<Vec<(String, String)>, String> {
    let current = read_crontab()?;
    let project_str = project_path.display().to_string();

    let mut entries = Vec::new();

    for line in current.lines() {
        if line.contains(&format!("gsd-cron:{}", project_str))
            && !line.starts_with('#')
        {
            // Parse: "M H * * * /path/wrapper.sh PHASE # gsd-cron:..."
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 7 {
                let time = format!("{}:{}", parts[1], parts[0]);
                let phase = parts[6].to_string();
                entries.push((phase, time));
            }
        }
    }

    Ok(entries)
}

/// Format crontab entries for display (without actually installing)
pub fn format_entries(entries: &[String]) -> String {
    entries.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;
    use crate::parser::{Phase, PhaseNumber, PhaseSchedulability, PhaseStatus};

    fn make_slot(hour: u32, min: u32, phases: Vec<(f64, &str)>) -> ScheduleSlot {
        ScheduleSlot {
            time: NaiveTime::from_hms_opt(hour, min, 0).unwrap(),
            phases: phases
                .into_iter()
                .map(|(num, name)| Phase {
                    number: PhaseNumber(num),
                    name: name.to_string(),
                    plans_complete: (0, 1),
                    status: PhaseStatus::NotStarted,
                    completed_date: None,
                    schedulability: PhaseSchedulability::Schedulable,
                    dir_path: None,
                })
                .collect(),
        }
    }

    #[test]
    fn test_generate_entries() {
        let slots = vec![
            make_slot(9, 0, vec![(1.0, "Foundation")]),
            make_slot(11, 0, vec![(2.0, "Auth")]),
            make_slot(13, 0, vec![(2.1, "Hotfix A"), (2.2, "Hotfix B")]),
            make_slot(15, 0, vec![(3.0, "API")]),
        ];

        let project = Path::new("/home/user/myproject");
        let wrapper = Path::new("/home/user/myproject/.planning/gsd-cron-wrapper.sh");

        let entries = generate_entries(&slots, project, wrapper);

        // First line is the tag
        assert!(entries[0].starts_with("# gsd-cron:"));

        // Check phase 1 entry
        assert!(entries[1].contains("0 9 * * *"));
        assert!(entries[1].contains("phase 1"));

        // Check phase 2 entry
        assert!(entries[2].contains("0 11 * * *"));

        // Check parallel phases
        assert!(entries[3].contains("0 13 * * *"));
        assert!(entries[3].contains("phase 2.1"));
        assert!(entries[4].contains("0 13 * * *"));
        assert!(entries[4].contains("phase 2.2"));

        // Last line is the END tag
        assert!(entries.last().unwrap().contains("END"));
    }

    #[test]
    fn test_remove_project_entries() {
        let crontab = r#"0 * * * * /some/other/job
# gsd-cron:/home/user/project
0 9 * * * /home/user/project/.planning/gsd-cron-wrapper.sh 1 # gsd-cron:/home/user/project phase 1
0 11 * * * /home/user/project/.planning/gsd-cron-wrapper.sh 2 # gsd-cron:/home/user/project phase 2
# gsd-cron:/home/user/project END
30 * * * * /another/job"#;

        let cleaned = remove_project_entries(crontab, Path::new("/home/user/project"));
        assert!(!cleaned.contains("gsd-cron"));
        assert!(cleaned.contains("/some/other/job"));
        assert!(cleaned.contains("/another/job"));
    }

    #[test]
    fn test_remove_preserves_other_projects() {
        let crontab = r#"# gsd-cron:/project-a
0 9 * * * /project-a/.planning/gsd-cron-wrapper.sh 1 # gsd-cron:/project-a phase 1
# gsd-cron:/project-a END
# gsd-cron:/project-b
0 9 * * * /project-b/.planning/gsd-cron-wrapper.sh 1 # gsd-cron:/project-b phase 1
# gsd-cron:/project-b END"#;

        let cleaned = remove_project_entries(crontab, Path::new("/project-a"));
        assert!(!cleaned.contains("project-a"));
        assert!(cleaned.contains("project-b"));
    }
}
