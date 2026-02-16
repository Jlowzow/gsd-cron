use std::path::Path;
use std::process::Command;

const TAG_PREFIX: &str = "# gsd-cron:";

/// Read the current user crontab
pub fn read_crontab() -> Result<String, String> {
    let output = Command::new("crontab")
        .arg("-l")
        .output()
        .map_err(|e| format!("Failed to read crontab: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
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

/// Install a single dispatcher crontab entry for a project.
/// Replaces any existing entries for this project with a single `gsd-cron run` entry.
/// Sources `~/.config/gsd-cron/env` if it exists (for ANTHROPIC_API_KEY).
pub fn install_dispatcher(
    project_path: &Path,
    binary_path: &Path,
    max_parallel: usize,
    interval_minutes: u32,
    window: Option<&str>,
    weekly_budget: Option<f64>,
) -> Result<(), String> {
    let current = read_crontab()?;
    let cleaned = remove_project_entries(&current, project_path);

    let project_str = project_path.display().to_string();
    let binary_str = binary_path.display().to_string();
    let log_file = project_path
        .join(".planning")
        .join("logs")
        .join("dispatcher.log");

    // Build cron schedule from interval
    let cron_schedule = interval_to_cron(interval_minutes);

    let window_arg = match window {
        Some(w) => format!(" --window {}", w),
        None => String::new(),
    };

    let budget_arg = match weekly_budget {
        Some(b) => format!(" --weekly-budget {:.2}", b),
        None => String::new(),
    };

    // Source env file if it exists, then run gsd-cron either way
    let env_source = "test -f ~/.config/gsd-cron/env && . ~/.config/gsd-cron/env;";

    let mut lines = Vec::new();
    lines.push(format!("{}{}", TAG_PREFIX, project_str));
    lines.push(format!(
        "{} {} {} run --project {} --max-parallel {}{}{} >> {} 2>&1 # gsd-cron:{}",
        cron_schedule, env_source, binary_str, project_str, max_parallel, window_arg, budget_arg, log_file.display(), project_str
    ));
    lines.push(format!("{}{} END", TAG_PREFIX, project_str));

    let mut final_content = cleaned;
    if !final_content.is_empty() && !final_content.ends_with('\n') {
        final_content.push('\n');
    }
    final_content.push_str(&lines.join("\n"));
    final_content.push('\n');

    write_crontab(&final_content)
}

/// Convert an interval in minutes to a cron schedule expression.
fn interval_to_cron(interval_minutes: u32) -> String {
    if interval_minutes == 0 {
        return "* * * * *".to_string();
    }

    if interval_minutes < 60 {
        // e.g. 30m -> */30 * * * *
        format!("*/{} * * * *", interval_minutes)
    } else if interval_minutes % 60 == 0 {
        let hours = interval_minutes / 60;
        // e.g. 2h -> 0 */2 * * *
        format!("0 */{} * * *", hours)
    } else {
        // Non-even hour intervals: just use minutes
        format!("*/{} * * * *", interval_minutes)
    }
}

/// Remove all crontab entries for a project
pub fn remove(project_path: &Path) -> Result<(), String> {
    let current = read_crontab()?;
    let cleaned = remove_project_entries(&current, project_path);

    if cleaned.trim().is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interval_to_cron_minutes() {
        assert_eq!(interval_to_cron(30), "*/30 * * * *");
        assert_eq!(interval_to_cron(15), "*/15 * * * *");
        assert_eq!(interval_to_cron(45), "*/45 * * * *");
    }

    #[test]
    fn test_interval_to_cron_hours() {
        assert_eq!(interval_to_cron(60), "0 */1 * * *");
        assert_eq!(interval_to_cron(120), "0 */2 * * *");
    }

    #[test]
    fn test_interval_to_cron_non_even() {
        assert_eq!(interval_to_cron(90), "*/90 * * * *");
    }

    #[test]
    fn test_remove_project_entries() {
        let crontab = r#"0 * * * * /some/other/job
# gsd-cron:/home/user/project
*/30 * * * * /usr/bin/gsd-cron run --project /home/user/project --max-parallel 2 >> /home/user/project/.planning/logs/dispatcher.log 2>&1 # gsd-cron:/home/user/project
# gsd-cron:/home/user/project END
30 * * * * /another/job"#;

        let cleaned = remove_project_entries(crontab, std::path::Path::new("/home/user/project"));
        assert!(!cleaned.contains("gsd-cron"));
        assert!(cleaned.contains("/some/other/job"));
        assert!(cleaned.contains("/another/job"));
    }

    #[test]
    fn test_remove_preserves_other_projects() {
        let crontab = r#"# gsd-cron:/project-a
*/30 * * * * /usr/bin/gsd-cron run --project /project-a --max-parallel 2 >> /project-a/.planning/logs/dispatcher.log 2>&1 # gsd-cron:/project-a
# gsd-cron:/project-a END
# gsd-cron:/project-b
*/30 * * * * /usr/bin/gsd-cron run --project /project-b --max-parallel 2 >> /project-b/.planning/logs/dispatcher.log 2>&1 # gsd-cron:/project-b
# gsd-cron:/project-b END"#;

        let cleaned = remove_project_entries(crontab, std::path::Path::new("/project-a"));
        assert!(!cleaned.contains("project-a"));
        assert!(cleaned.contains("project-b"));
    }
}
