mod crontab;
mod parser;
mod scheduler;
mod wrapper;

use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "gsd-cron")]
#[command(about = "Crontab scheduler for GSD phase execution")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate crontab entries (prints to stdout)
    Generate {
        /// Path to the GSD project root
        #[arg(long)]
        project: PathBuf,

        /// Start time for the first phase (HH:MM format)
        #[arg(long, default_value = "09:00")]
        start: String,

        /// Interval between dependent phases (e.g., 2h, 30m, 1h30m)
        #[arg(long, default_value = "2h")]
        interval: String,
    },

    /// Generate and install crontab entries
    Install {
        /// Path to the GSD project root
        #[arg(long)]
        project: PathBuf,

        /// Start time for the first phase (HH:MM format)
        #[arg(long, default_value = "09:00")]
        start: String,

        /// Interval between dependent phases (e.g., 2h, 30m, 1h30m)
        #[arg(long, default_value = "2h")]
        interval: String,
    },

    /// Show status of scheduled, completed, skipped, and blocked phases
    Status {
        /// Path to the GSD project root
        #[arg(long)]
        project: PathBuf,
    },

    /// Remove all crontab entries for a project
    Remove {
        /// Path to the GSD project root
        #[arg(long)]
        project: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Generate {
            project,
            start,
            interval,
        } => cmd_generate(&project, &start, &interval),
        Commands::Install {
            project,
            start,
            interval,
        } => cmd_install(&project, &start, &interval),
        Commands::Status { project } => cmd_status(&project),
        Commands::Remove { project } => cmd_remove(&project),
    }
}

fn load_phases(project: &PathBuf) -> Vec<parser::Phase> {
    let planning_dir = project.join(".planning");

    // Parse ROADMAP.md
    let roadmap_path = planning_dir.join("ROADMAP.md");
    let roadmap_content = match fs::read_to_string(&roadmap_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading ROADMAP.md: {}", e);
            std::process::exit(1);
        }
    };

    let mut phases = parser::parse_roadmap(&roadmap_content);

    if phases.is_empty() {
        eprintln!("No phases found in ROADMAP.md");
        std::process::exit(1);
    }

    // Discover phase directories
    let phase_dirs = parser::discover_phase_dirs(&planning_dir);

    // Determine schedulability for each phase
    for phase in &mut phases {
        parser::determine_schedulability(phase, &phase_dirs);
    }

    phases
}

fn cmd_generate(project: &PathBuf, start: &str, interval: &str) {
    let start_time = match scheduler::parse_start_time(start) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let interval_minutes = match scheduler::parse_interval(interval) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let phases = load_phases(project);
    let schedule = scheduler::build_schedule(&phases, start_time, interval_minutes);

    // Generate and write wrapper script
    let wrapper_path = wrapper::wrapper_script_path(project);
    let wrapper_content = wrapper::generate_wrapper_script(project);

    if let Some(parent) = wrapper_path.parent() {
        fs::create_dir_all(parent).ok();
    }

    match fs::write(&wrapper_path, &wrapper_content) {
        Ok(_) => {
            // Make executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = fs::Permissions::from_mode(0o755);
                fs::set_permissions(&wrapper_path, perms).ok();
            }
            eprintln!("Wrapper script written to: {}", wrapper_path.display());
        }
        Err(e) => {
            eprintln!("Warning: Could not write wrapper script: {}", e);
        }
    }

    // Create logs directory
    let logs_dir = project.join(".planning").join("logs");
    fs::create_dir_all(&logs_dir).ok();

    // Print crontab entries
    let entries = crontab::generate_entries(&schedule.slots, project, &wrapper_path);
    println!("{}", crontab::format_entries(&entries));

    // Print warnings about skipped phases
    if !schedule.skipped.is_empty() {
        eprintln!();
        eprintln!("Skipped phases:");
        for (phase, reason) in &schedule.skipped {
            eprintln!("  Phase {}: {} — {}", phase.number, phase.name, reason);
        }
    }

    // Print schedule summary
    if !schedule.slots.is_empty() {
        eprintln!();
        eprintln!("Schedule:");
        for slot in &schedule.slots {
            let phase_names: Vec<String> = slot
                .phases
                .iter()
                .map(|p| format!("{} ({})", p.number, p.name))
                .collect();
            eprintln!("  {} — {}", slot.time.format("%H:%M"), phase_names.join(", "));
        }
    }
}

fn cmd_install(project: &PathBuf, start: &str, interval: &str) {
    let start_time = match scheduler::parse_start_time(start) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let interval_minutes = match scheduler::parse_interval(interval) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let phases = load_phases(project);
    let schedule = scheduler::build_schedule(&phases, start_time, interval_minutes);

    if schedule.slots.is_empty() {
        eprintln!("No schedulable phases found. Nothing to install.");
        if !schedule.skipped.is_empty() {
            eprintln!();
            eprintln!("Skipped phases:");
            for (phase, reason) in &schedule.skipped {
                eprintln!("  Phase {}: {} — {}", phase.number, phase.name, reason);
            }
        }
        return;
    }

    // Generate and write wrapper script
    let wrapper_path = wrapper::wrapper_script_path(project);
    let wrapper_content = wrapper::generate_wrapper_script(project);

    if let Some(parent) = wrapper_path.parent() {
        fs::create_dir_all(parent).ok();
    }

    match fs::write(&wrapper_path, &wrapper_content) {
        Ok(_) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = fs::Permissions::from_mode(0o755);
                fs::set_permissions(&wrapper_path, perms).ok();
            }
            eprintln!("Wrapper script written to: {}", wrapper_path.display());
        }
        Err(e) => {
            eprintln!("Error: Could not write wrapper script: {}", e);
            std::process::exit(1);
        }
    }

    // Create logs directory
    let logs_dir = project.join(".planning").join("logs");
    fs::create_dir_all(&logs_dir).ok();

    // Install to crontab
    match crontab::install(&schedule.slots, project, &wrapper_path) {
        Ok(_) => {
            eprintln!("Crontab entries installed successfully.");
        }
        Err(e) => {
            eprintln!("Error installing crontab: {}", e);
            std::process::exit(1);
        }
    }

    // Print schedule
    eprintln!();
    eprintln!("Scheduled phases:");
    for slot in &schedule.slots {
        let phase_names: Vec<String> = slot
            .phases
            .iter()
            .map(|p| format!("{} ({})", p.number, p.name))
            .collect();
        eprintln!("  {} — {}", slot.time.format("%H:%M"), phase_names.join(", "));
    }

    if !schedule.skipped.is_empty() {
        eprintln!();
        eprintln!("Skipped phases:");
        for (phase, reason) in &schedule.skipped {
            eprintln!("  Phase {}: {} — {}", phase.number, phase.name, reason);
        }
    }
}

fn cmd_status(project: &PathBuf) {
    let phases = load_phases(project);
    let planning_dir = project.join(".planning");
    let phase_dirs = parser::discover_phase_dirs(&planning_dir);

    // Check what's in crontab
    let scheduled = crontab::get_scheduled_phases(project).unwrap_or_default();

    println!("GSD Phase Status: {}", project.display());
    println!("{}", "=".repeat(60));
    println!();

    for phase in &phases {
        let padded = phase.number.padded();
        let status_icon = match phase.schedulability {
            parser::PhaseSchedulability::AlreadyComplete => "COMPLETE",
            parser::PhaseSchedulability::Schedulable => {
                // Check if it's in crontab
                if scheduled.iter().any(|(p, _)| *p == phase.number.display()) {
                    "SCHEDULED"
                } else {
                    "READY"
                }
            }
            parser::PhaseSchedulability::NeedsHuman => "CHECKPOINT",
            parser::PhaseSchedulability::NeedsPlanning => "NEEDS PLANNING",
            parser::PhaseSchedulability::NeedsDiscussionOrPlanning => "NEEDS DISCUSSION",
        };

        // Check verification status
        let verification = if let Some(dir) = phase_dirs.get(&padded) {
            if parser::has_passing_verification(dir, &phase.number) {
                " [verified]"
            } else {
                ""
            }
        } else {
            ""
        };

        let sched_time = scheduled
            .iter()
            .find(|(p, _)| *p == phase.number.display())
            .map(|(_, t)| format!(" @ {}", t))
            .unwrap_or_default();

        println!(
            "  Phase {:>5}: {:<30} [{:<16}]{}{}",
            phase.number.display(),
            phase.name,
            status_icon,
            verification,
            sched_time,
        );
    }

    println!();
}

fn cmd_remove(project: &PathBuf) {
    match crontab::remove(project) {
        Ok(_) => {
            eprintln!("Crontab entries removed for: {}", project.display());

            // Clean up wrapper script
            let wrapper_path = wrapper::wrapper_script_path(project);
            if wrapper_path.exists() {
                fs::remove_file(&wrapper_path).ok();
                eprintln!("Wrapper script removed: {}", wrapper_path.display());
            }
        }
        Err(e) => {
            eprintln!("Error removing crontab entries: {}", e);
            std::process::exit(1);
        }
    }
}
