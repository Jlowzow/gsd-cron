mod crontab;
mod parser;
mod runner;
mod scheduler;

use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::fs;
use std::io::BufRead;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "gsd-cron")]
#[command(about = "Dynamic dispatcher for GSD phase execution")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the dispatcher — evaluates phase readiness and executes in parallel
    Run {
        /// Path to the GSD project root
        #[arg(long)]
        project: PathBuf,

        /// Maximum number of phases to execute in parallel
        #[arg(long, default_value = "2")]
        max_parallel: usize,

        /// Restrict execution to a time window (e.g., 23:00-05:00)
        #[arg(long)]
        window: Option<String>,

        /// Weekly spending limit in USD (e.g., 5.00)
        #[arg(long)]
        weekly_budget: Option<f64>,
    },

    /// Install a crontab entry to run the dispatcher periodically
    Install {
        /// Path to the GSD project root
        #[arg(long)]
        project: PathBuf,

        /// How often to run the dispatcher (e.g., 30m, 1h, 2h)
        #[arg(long, default_value = "30m")]
        every: String,

        /// Maximum number of phases to execute in parallel
        #[arg(long, default_value = "2")]
        max_parallel: usize,

        /// Restrict execution to a time window (e.g., 23:00-05:00)
        #[arg(long)]
        window: Option<String>,

        /// Weekly spending limit in USD (e.g., 5.00)
        #[arg(long)]
        weekly_budget: Option<f64>,
    },

    /// Show status of all phases with dynamic readiness labels
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

    /// Store an Anthropic admin key for cost tracking
    SetupKey {},
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            project,
            max_parallel,
            window,
            weekly_budget,
        } => cmd_run(&project, max_parallel, window.as_deref(), weekly_budget),
        Commands::Install {
            project,
            every,
            max_parallel,
            window,
            weekly_budget,
        } => cmd_install(&project, &every, max_parallel, window.as_deref(), weekly_budget),
        Commands::Status { project } => cmd_status(&project),
        Commands::Remove { project } => cmd_remove(&project),
        Commands::SetupKey {} => cmd_setup_key(),
    }
}

fn load_phases(project: &PathBuf) -> (Vec<parser::Phase>, HashMap<String, PathBuf>) {
    let planning_dir = project.join(".planning");

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

    let phase_dirs = parser::discover_phase_dirs(&planning_dir);

    for phase in &mut phases {
        parser::determine_schedulability(phase, &phase_dirs);
    }

    (phases, phase_dirs)
}

fn cmd_run(project: &PathBuf, max_parallel: usize, window: Option<&str>, weekly_budget: Option<f64>) {
    if let Some(w) = window {
        if let Err(e) = runner::parse_window(w) {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
    runner::run(project, max_parallel, window, weekly_budget);
}

fn cmd_install(project: &PathBuf, every: &str, max_parallel: usize, window: Option<&str>, weekly_budget: Option<f64>) {
    if let Some(w) = window {
        if let Err(e) = runner::parse_window(w) {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
    let interval_minutes = match scheduler::parse_interval(every) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Find our binary path
    let binary_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: could not determine binary path: {}", e);
            std::process::exit(1);
        }
    };

    // Create logs directory
    let logs_dir = project.join(".planning").join("logs");
    fs::create_dir_all(&logs_dir).ok();

    match crontab::install_dispatcher(project, &binary_path, max_parallel, interval_minutes, window, weekly_budget) {
        Ok(_) => {
            eprintln!("Dispatcher crontab entry installed.");
            let window_info = match window {
                Some(w) => format!(" --window {}", w),
                None => String::new(),
            };
            let budget_info = match weekly_budget {
                Some(b) => format!(" --weekly-budget {:.2}", b),
                None => String::new(),
            };
            eprintln!(
                "  Runs every {} minutes: gsd-cron run --project {} --max-parallel {}{}{}",
                interval_minutes,
                project.display(),
                max_parallel,
                window_info,
                budget_info
            );
        }
        Err(e) => {
            eprintln!("Error installing crontab: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_setup_key() {
    eprintln!("Enter your Anthropic admin API key (sk-ant-admin...):");

    let stdin = std::io::stdin();
    let line = match stdin.lock().lines().next() {
        Some(Ok(l)) => l.trim().to_string(),
        _ => {
            eprintln!("Error: could not read key from stdin");
            std::process::exit(1);
        }
    };

    if line.is_empty() {
        eprintln!("Error: empty key");
        std::process::exit(1);
    }

    if !line.starts_with("sk-ant-admin") {
        eprintln!("Error: key must be an admin key (starts with 'sk-ant-admin').");
        eprintln!("Admin keys are required for the Cost API used by --weekly-budget.");
        eprintln!("Generate one at: https://console.anthropic.com/settings/admin-keys");
        std::process::exit(1);
    }

    let config_dir = dirs_or_home().join(".config").join("gsd-cron");
    if let Err(e) = fs::create_dir_all(&config_dir) {
        eprintln!("Error creating config directory: {}", e);
        std::process::exit(1);
    }

    let env_path = config_dir.join("env");
    let content = format!("export ADMIN_API_KEY={}\n", line);

    if let Err(e) = fs::write(&env_path, &content) {
        eprintln!("Error writing env file: {}", e);
        std::process::exit(1);
    }

    if let Err(e) = fs::set_permissions(&env_path, fs::Permissions::from_mode(0o600)) {
        eprintln!("Warning: could not set permissions on {}: {}", env_path.display(), e);
    }

    eprintln!("Admin key saved to {}", env_path.display());
    eprintln!("The cron dispatcher will source this file for --weekly-budget cost checks.");
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn cmd_status(project: &PathBuf) {
    let (phases, phase_dirs) = load_phases(project);

    println!("GSD Phase Status: {}", project.display());
    println!("{}", "=".repeat(60));
    println!();

    for phase in &phases {
        let label = runner::readiness_label(phase, &phases, &phase_dirs);

        println!(
            "  Phase {:>5}: {:<30} [{:<16}]",
            phase.number.display(),
            phase.name,
            label,
        );
    }

    println!();
}

fn cmd_remove(project: &PathBuf) {
    match crontab::remove(project) {
        Ok(_) => {
            eprintln!("Crontab entries removed for: {}", project.display());
        }
        Err(e) => {
            eprintln!("Error removing crontab entries: {}", e);
            std::process::exit(1);
        }
    }
}
