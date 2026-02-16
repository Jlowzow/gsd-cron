use crate::parser::{Phase, PhaseSchedulability};
use chrono::{NaiveTime, Timelike};

#[derive(Debug, Clone)]
pub struct ScheduleSlot {
    pub time: NaiveTime,
    pub phases: Vec<Phase>,
}

#[derive(Debug)]
pub struct Schedule {
    pub slots: Vec<ScheduleSlot>,
    pub skipped: Vec<(Phase, String)>,
}

/// Build a dependency-aware schedule from parsed phases.
///
/// Rules:
/// - Sequential integer phases depend on the previous integer phase
/// - Decimal phases (e.g., 2.1, 2.2) depend on parent integer (2) but not each other
/// - Independent phases get same time slot
/// - Dependent phases are staggered by interval
/// - Complete/unschedulable phases are skipped
pub fn build_schedule(
    phases: &[Phase],
    start_time: NaiveTime,
    interval_minutes: u32,
) -> Schedule {
    let mut slots: Vec<ScheduleSlot> = Vec::new();
    let mut skipped: Vec<(Phase, String)> = Vec::new();

    // Filter to schedulable phases only, tracking skipped ones
    let mut schedulable: Vec<&Phase> = Vec::new();
    for phase in phases {
        match phase.schedulability {
            PhaseSchedulability::Schedulable => {
                schedulable.push(phase);
            }
            PhaseSchedulability::AlreadyComplete => {
                skipped.push((phase.clone(), "Already complete".to_string()));
            }
            PhaseSchedulability::NeedsHuman => {
                skipped.push((
                    phase.clone(),
                    "Has checkpoint requiring human input (autonomous: false)".to_string(),
                ));
            }
            PhaseSchedulability::NeedsPlanning => {
                skipped.push((
                    phase.clone(),
                    "Has context but no plans yet (needs planning)".to_string(),
                ));
            }
            PhaseSchedulability::NeedsDiscussionOrPlanning => {
                skipped.push((
                    phase.clone(),
                    "No plans or context (needs discussion/planning)".to_string(),
                ));
            }
        }
    }

    if schedulable.is_empty() {
        return Schedule { slots, skipped };
    }

    // Assign dependency levels (slot indices)
    // Each phase gets a level based on its dependencies
    let phase_levels = assign_levels(&schedulable);

    // Group phases by level
    let max_level = phase_levels.iter().map(|(_, l)| *l).max().unwrap_or(0);

    for level in 0..=max_level {
        let phases_at_level: Vec<Phase> = phase_levels
            .iter()
            .filter(|(_, l)| *l == level)
            .map(|(p, _)| (*p).clone())
            .collect();

        if !phases_at_level.is_empty() {
            let minutes_offset = level * interval_minutes;
            let slot_time = add_minutes(start_time, minutes_offset);

            slots.push(ScheduleSlot {
                time: slot_time,
                phases: phases_at_level,
            });
        }
    }

    Schedule { slots, skipped }
}

/// Assign dependency levels to phases.
/// Level 0 = no dependencies or all deps already complete.
/// Each level increments by 1 for each dependency chain step.
fn assign_levels<'a>(phases: &[&'a Phase]) -> Vec<(&'a Phase, u32)> {
    // Sort all phases by number
    let mut sorted: Vec<&Phase> = phases.to_vec();
    sorted.sort_by(|a, b| a.number.partial_cmp(&b.number).unwrap());

    // Collect decimal phases grouped by parent integer
    let mut decimals_for: std::collections::HashMap<u32, Vec<&Phase>> =
        std::collections::HashMap::new();
    for p in &sorted {
        if p.number.is_decimal() {
            decimals_for
                .entry(p.number.parent_integer())
                .or_default()
                .push(p);
        }
    }

    // Walk through sorted integer phases, assigning levels.
    // After each integer phase, if there are decimal children, they get the next level,
    // and the following integer phase gets the level after that.
    let mut result: Vec<(&Phase, u32)> = Vec::new();
    let mut current_level: u32 = 0;

    let int_phases: Vec<&&Phase> = sorted.iter().filter(|p| !p.number.is_decimal()).collect();

    for (i, phase) in int_phases.iter().enumerate() {
        let n = phase.number.0 as u32;

        if i > 0 {
            current_level += 1;
        }

        result.push((phase, current_level));

        // Check if there are decimal phases after this integer
        if let Some(dec_phases) = decimals_for.get(&n) {
            current_level += 1;
            for dp in dec_phases {
                result.push((dp, current_level));
            }
        }
    }

    // Handle orphan decimals whose parent integer isn't in the schedulable set
    for p in &sorted {
        if p.number.is_decimal() {
            let parent = p.number.parent_integer();
            let already_assigned = result.iter().any(|(rp, _)| {
                std::ptr::eq(*rp as *const Phase, *p as *const Phase)
            });
            if !already_assigned {
                // Place after the closest preceding integer phase's level
                let level = result
                    .iter()
                    .filter(|(rp, _)| !rp.number.is_decimal() && rp.number.0 as u32 <= parent)
                    .map(|(_, l)| *l + 1)
                    .max()
                    .unwrap_or(0);
                result.push((p, level));
            }
        }
    }

    result
}

fn add_minutes(time: NaiveTime, minutes: u32) -> NaiveTime {
    let total_seconds = time.num_seconds_from_midnight() + (minutes as u32) * 60;
    // Wrap around at 24h
    let wrapped = total_seconds % (24 * 3600);
    NaiveTime::from_num_seconds_from_midnight_opt(wrapped, 0)
        .unwrap_or(time)
}

/// Parse an interval string like "2h", "30m", "1h30m", "90m" into minutes
pub fn parse_interval(s: &str) -> Result<u32, String> {
    let s = s.trim().to_lowercase();

    // Try combined first: "1h30m"
    let re = regex::Regex::new(r"^(\d+)h(\d+)m$").unwrap();
    if let Some(cap) = re.captures(&s) {
        let hours: u32 = cap[1].parse().map_err(|_| format!("Invalid interval: {}", s))?;
        let mins: u32 = cap[2].parse().map_err(|_| format!("Invalid interval: {}", s))?;
        return Ok(hours * 60 + mins);
    }

    // Try pure hours: "2h"
    if let Some(stripped) = s.strip_suffix('h') {
        if let Ok(hours) = stripped.parse::<u32>() {
            return Ok(hours * 60);
        }
    }

    // Try pure minutes: "90m"
    if let Some(stripped) = s.strip_suffix('m') {
        return stripped
            .parse::<u32>()
            .map_err(|_| format!("Invalid interval: {}", s));
    }

    // Try plain number as minutes
    s.parse::<u32>()
        .map_err(|_| format!("Invalid interval '{}'. Use formats like: 2h, 30m, 1h30m", s))
}

/// Parse a time string like "09:00" or "14:30"
pub fn parse_start_time(s: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M")
        .map_err(|e| format!("Invalid time '{}': {}. Use HH:MM format.", s, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{PhaseNumber, PhaseSchedulability, PhaseStatus};

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
    fn test_simple_sequential_schedule() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(3.0, "API", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];

        let start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let schedule = build_schedule(&phases, start, 120);

        assert_eq!(schedule.slots.len(), 3);
        assert_eq!(schedule.slots[0].time, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert_eq!(schedule.slots[0].phases.len(), 1);
        assert_eq!(schedule.slots[0].phases[0].name, "Foundation");

        assert_eq!(schedule.slots[1].time, NaiveTime::from_hms_opt(11, 0, 0).unwrap());
        assert_eq!(schedule.slots[1].phases[0].name, "Auth");

        assert_eq!(schedule.slots[2].time, NaiveTime::from_hms_opt(13, 0, 0).unwrap());
        assert_eq!(schedule.slots[2].phases[0].name, "API");
    }

    #[test]
    fn test_parallel_decimal_phases() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(2.1, "Hotfix A", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(2.2, "Hotfix B", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(3.0, "API", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];

        let start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let schedule = build_schedule(&phases, start, 120);

        // Expect: slot 0 = phase 1, slot 1 = phase 2, slot 2 = 2.1 + 2.2 (parallel), slot 3 = phase 3
        assert_eq!(schedule.slots.len(), 4);

        assert_eq!(schedule.slots[0].phases.len(), 1);
        assert_eq!(schedule.slots[0].phases[0].number.display(), "1");

        assert_eq!(schedule.slots[1].phases.len(), 1);
        assert_eq!(schedule.slots[1].phases[0].number.display(), "2");

        // Decimal phases should be in the same slot (parallel)
        assert_eq!(schedule.slots[2].phases.len(), 2);
        let slot2_names: Vec<String> = schedule.slots[2].phases.iter().map(|p| p.number.display()).collect();
        assert!(slot2_names.contains(&"2.1".to_string()));
        assert!(slot2_names.contains(&"2.2".to_string()));

        assert_eq!(schedule.slots[3].phases.len(), 1);
        assert_eq!(schedule.slots[3].phases[0].number.display(), "3");
    }

    #[test]
    fn test_skips_complete_and_human_phases() {
        let phases = vec![
            make_phase(1.0, "Foundation", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
            make_phase(2.0, "Auth", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
            make_phase(3.0, "Manual", PhaseStatus::NotStarted, PhaseSchedulability::NeedsHuman),
            make_phase(4.0, "API", PhaseStatus::NotStarted, PhaseSchedulability::Schedulable),
        ];

        let start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let schedule = build_schedule(&phases, start, 120);

        // Only schedulable phases get slots
        assert_eq!(schedule.slots.len(), 2);
        assert_eq!(schedule.slots[0].phases[0].name, "Auth");
        assert_eq!(schedule.slots[1].phases[0].name, "API");

        // Skipped phases recorded
        assert_eq!(schedule.skipped.len(), 2);
    }

    #[test]
    fn test_schedule_with_only_complete_phases() {
        let phases = vec![
            make_phase(1.0, "Done", PhaseStatus::Complete, PhaseSchedulability::AlreadyComplete),
        ];

        let start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let schedule = build_schedule(&phases, start, 120);

        assert_eq!(schedule.slots.len(), 0);
        assert_eq!(schedule.skipped.len(), 1);
    }

    #[test]
    fn test_parse_interval() {
        assert_eq!(parse_interval("2h").unwrap(), 120);
        assert_eq!(parse_interval("30m").unwrap(), 30);
        assert_eq!(parse_interval("1h30m").unwrap(), 90);
        assert_eq!(parse_interval("90").unwrap(), 90);
        assert!(parse_interval("abc").is_err());
    }

    #[test]
    fn test_parse_start_time() {
        let t = parse_start_time("09:00").unwrap();
        assert_eq!(t, NaiveTime::from_hms_opt(9, 0, 0).unwrap());

        let t = parse_start_time("14:30").unwrap();
        assert_eq!(t, NaiveTime::from_hms_opt(14, 30, 0).unwrap());

        assert!(parse_start_time("invalid").is_err());
    }

    #[test]
    fn test_time_wrapping() {
        let t = NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        let result = add_minutes(t, 120);
        assert_eq!(result, NaiveTime::from_hms_opt(1, 0, 0).unwrap());
    }
}
