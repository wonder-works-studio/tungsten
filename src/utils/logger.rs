use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Turn verbose (debug) logging on/off. Called once from main().
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

/// True when debug-level messages should be printed.
pub fn verbose_enabled() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

#[macro_export]
macro_rules! log {
    ($level:ident, $($arg:tt)*) => {{
        use colored::Colorize;
        let level = stringify!($level);
        let emit = level != "debug" || $crate::utils::logger::verbose_enabled();
        if emit {
            let msg = format!($($arg)*);
            match level {
                "debug"   => println!("{} {}", " DEBUG   ".on_blue().black().bold(), msg.blue()),
                "info"    => println!("{} {}", " INFO    ".on_white().black().bold(), msg.white()),
                "success" => println!("{} {}", " SUCCESS ".on_bright_green().green().bold(), msg.green()),
                "warn"    => println!("{} {}", " WARNING ".on_bright_yellow().yellow().bold(), msg.yellow()),
                "error"   => println!("{} {}", " ERROR   ".on_bright_red().red().bold(), msg.red()),
                "section" => {
                    let width = crossterm::terminal::size()
                        .map(|(w, _)| w as usize)
                        .unwrap_or(120);
                    let padded = format!(" {}", msg);
                    println!("\n{}", format!("{:<width$}", padded, width = width).blue().bold().on_cyan());
                }
                _         => println!("{}", msg),
            }
        }
    }};
}

const BAR_WIDTH: usize = 28;
/// Max width of the phase label column (padded with spaces).
const PHASE_WIDTH: usize = 12;
/// Max width of the item name shown at the right of the bar.
const ITEM_WIDTH: usize = 36;
/// Label prefix on the completion line, colored separately from the rest.
const SUCCESS_LABEL: &str = " SUCCESS ";

/// Render a labelled progress bar on the current line (no newline unless done).
///
/// ```text
/// Uploading    [████████░░░░░░░░░░░░░░░░░░░░]  40%   2/5   my_icon.png
/// ```
///
/// When `current >= total` the line is finalized with a newline.
pub fn progress(phase: &str, current: usize, total: usize, item: &str) {
    use colored::Colorize;

    if current >= total {
        // The in-progress line is longer than this one (it carries an item
        // name), so clear the row first — otherwise its tail stays visible
        // past the end of the completion line.
        clear_progress_line();
        let done = progress_done_line(phase, total);
        let (label, rest) = done.split_at(SUCCESS_LABEL.len());
        println!("{}{}", label.on_green().black().bold(), rest.cyan().bold());
    } else {
        print!("\r{}", progress_line(phase, current, total, item).cyan());
        std::io::stdout()
            .flush()
            .expect("Failed to flush stdout during progress update (this may indicate a system I/O error or broken pipe)");
    }
}

/// Truncate `item` to `ITEM_WIDTH`, keeping the tail (the distinctive part of
/// a path) and marking the elision with a leading ellipsis.
fn truncate_item(item: &str) -> String {
    let count = item.chars().count();
    if count <= ITEM_WIDTH {
        return item.to_string();
    }
    // Skip by chars so a multi-byte name can't be split mid-character.
    let tail: String = item.chars().skip(count - (ITEM_WIDTH - 1)).collect();
    format!("…{}", tail)
}

/// Build the uncolored progress line. Column widths are applied here, on plain
/// text — padding a colorized string instead would count ANSI escape bytes
/// toward the width and collapse the alignment.
fn progress_line(phase: &str, current: usize, total: usize, item: &str) -> String {
    let ratio = if total == 0 {
        0.0f32
    } else {
        current as f32 / total as f32
    };
    let filled = (ratio * BAR_WIDTH as f32).floor() as usize;
    let percent = (ratio * 100.0).floor() as usize;
    let num_width = total.to_string().len();

    format!(
        "{:<phase_w$} [{}{}] {:>3}%  {:>num_w$}/{:>num_w$}  {:<item_w$}",
        phase,
        "█".repeat(filled),
        "░".repeat(BAR_WIDTH - filled),
        percent,
        current,
        total,
        truncate_item(item),
        phase_w = PHASE_WIDTH,
        item_w = ITEM_WIDTH,
        num_w = num_width,
    )
}

/// Build the uncolored completion line shown once a phase reaches `total`.
fn progress_done_line(phase: &str, total: usize) -> String {
    format!(
        "{} {:<phase_w$} [{}] 100%  {}/{}",
        SUCCESS_LABEL,
        phase,
        "█".repeat(BAR_WIDTH),
        total,
        total,
        phase_w = PHASE_WIDTH,
    )
}

/// Clear the current progress line (call before printing a warn/error mid-progress).
pub fn clear_progress_line() {
    // Overwrite the full terminal row so no tail of a longer line survives.
    let width = crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(120)
        .max(120);
    print!("\r{:<width$}\r", "", width = width);
    std::io::stdout()
        .flush()
        .expect("Failed to flush stdout while clearing progress line (this may indicate a system I/O error or broken pipe)");
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_line_separates_counter_from_item() {
        let line = progress_line("Uploading", 3, 3, "NeonCitySecretRewardQuiet");
        assert!(
            line.contains("3/3  NeonCitySecretRewardQuiet"),
            "counter and item must stay separated: {:?}",
            line
        );
    }

    #[test]
    fn test_progress_line_pads_columns_to_fixed_widths() {
        let short = progress_line("Packing", 1, 2, "a");
        let long = progress_line("Transcoding", 1, 2, "b");
        // Both phases pad to PHASE_WIDTH, so the bars start at the same column.
        assert_eq!(
            short.find('['),
            long.find('['),
            "bars must align:\n{:?}\n{:?}",
            short,
            long
        );
        // The item column is padded out, so a short item still clears the row.
        assert!(short.ends_with(' '), "item column must pad: {:?}", short);
    }

    #[test]
    fn test_progress_line_counter_widths_match() {
        // Counters are right-aligned to the total's width so nothing shifts
        // as the current value grows.
        let early = progress_line("Uploading", 9, 100, "x");
        let late = progress_line("Uploading", 100, 100, "x");
        assert!(early.contains("  9/100"), "{:?}", early);
        assert_eq!(early.len(), late.len(), "{:?}\n{:?}", early, late);
    }

    #[test]
    fn test_done_line_reports_totals_without_item() {
        let done = progress_done_line("Uploading", 7);
        assert!(done.starts_with(SUCCESS_LABEL), "{:?}", done);
        assert!(done.contains("100%  7/7"), "{:?}", done);
        assert!(done.ends_with("7/7"), "no item name trails: {:?}", done);
    }

    #[test]
    fn test_truncate_item_keeps_tail() {
        let long = "a".repeat(ITEM_WIDTH + 10);
        let truncated = truncate_item(&long);
        assert_eq!(truncated.chars().count(), ITEM_WIDTH);
        assert!(truncated.starts_with('…'));

        let short = "icons/home.png";
        assert_eq!(truncate_item(short), short);
    }

    #[test]
    fn test_truncate_item_handles_multibyte_names() {
        // Truncation must land on a char boundary, not split a code point.
        let name = "日本語".repeat(20);
        let truncated = truncate_item(&name);
        assert_eq!(truncated.chars().count(), ITEM_WIDTH);
    }
}
