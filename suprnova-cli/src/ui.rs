use console::{style, Style, Term};

// ─── Brand colors ───────────────────────────────────────────
// Suprnova's palette: warm explosion (yellow/orange core) with cool edges (cyan/blue)
const BANNER: &str = r#"
  ▄▄▄▄▄                                           
 ██▀▀▀▀█▄                                         
 ▀██▄  ▄▀             ▄    ▄                      
   ▀██▄▄  ██ ██ ████▄ ████▄████▄ ▄███▄▀█▄ ██▀▄▀▀█▄
 ▄   ▀██▄ ██ ██ ██ ██ ██   ██ ██ ██ ██ ██▄██ ▄█▀██
 ▀██████▀▄▀██▀█▄████▀▄█▀  ▄██ ▀█▄▀███▀  ▀█▀ ▄▀█▄██
                ██                                
                ▀                                 
"#;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Print the Suprnova banner with version
pub fn banner() {
    let term = Term::stdout();
    let _ = term.clear_line();

    for (i, line) in BANNER.lines().enumerate() {
        if line.is_empty() { continue; }
        match i {
            1     => println!("{}", style(line).color256(220).bold()), // bright yellow
            2     => println!("{}", style(line).color256(214).bold()), // yellow-orange
            3     => println!("{}", style(line).color256(214).bold()), // orange
            4     => println!("{}", style(line).color256(208).bold()), // deeper orange
            5     => println!("{}", style(line).color256(203).bold()), // red-orange
            6     => println!("{}", style(line).color256(197).bold()), // red
            7 | 8 => println!("{}", style(line).color256(161).bold()), // deep magenta tail
            _     => println!("{}", style(line).cyan()),
        }
    }
    println!(
        "  {} {}",
        style("A Rust web framework that doesn't gatekeep.").dim(),
        style(format!("v{}", VERSION)).dim().italic(),
    );
    println!();
}

/// Section header — used to group related output
pub fn header(text: &str) {
    println!();
    println!("  {}", style(text).cyan().bold().underlined());
    println!();
}

/// Success: ✓ message
pub fn success(msg: &str) {
    println!("  {} {}", style("✓").green().bold(), msg);
}

/// Error: ✗ message  
pub fn error(msg: &str) {
    eprintln!("  {} {}", style("✗").red().bold(), msg);
}

/// Warning: ⚠ message
pub fn warning(msg: &str) {
    eprintln!("  {} {}", style("⚠").yellow().bold(), msg);
}

/// Info: → message
pub fn info(msg: &str) {
    println!("  {} {}", style("→").cyan(), msg);
}

/// Step indicator: [n/total] message
pub fn step(current: usize, total: usize, msg: &str) {
    println!(
        "  {} {}",
        style(format!("[{}/{}]", current, total)).dim().bold(),
        msg,
    );
}

/// Dimmed hint text
pub fn hint(msg: &str) {
    println!("  {}", style(msg).dim());
}

/// A labeled value line:  label ··· value
pub fn label_value(label: &str, value: &str) {
    let dots = ".".repeat(36_usize.saturating_sub(label.len()));
    println!(
        "  {} {} {}",
        style(label).bold(),
        style(dots).dim(),
        style(value).cyan(),
    );
}

/// Print a boxed summary panel
pub fn panel(title: &str, lines: &[&str]) {
    let max_len = lines.iter().map(|l| console::measure_text_width(l)).max().unwrap_or(40);
    let width = max_len.max(console::measure_text_width(title) + 4) + 4;

    let border = Style::new().dim();

    // Top border
    println!(
        "  {}{}{}",
        border.apply_to("╭─"),
        border.apply_to("─".repeat(width)),
        border.apply_to("─╮"),
    );

    // Title
    let title_pad = width - console::measure_text_width(title);
    println!(
        "  {}  {}{}{}",
        border.apply_to("│"),
        style(title).bold().cyan(),
        " ".repeat(title_pad),
        border.apply_to("│"),
    );

    // Separator
    println!(
        "  {}{}{}",
        border.apply_to("├─"),
        border.apply_to("─".repeat(width)),
        border.apply_to("─┤"),
    );

    // Content lines
    for line in lines {
        let pad = width - console::measure_text_width(line);
        println!(
            "  {}  {}{}{}",
            border.apply_to("│"),
            line,
            " ".repeat(pad),
            border.apply_to("│"),
        );
    }

    // Bottom border
    println!(
        "  {}{}{}",
        border.apply_to("╰─"),
        border.apply_to("─".repeat(width)),
        border.apply_to("─╯"),
    );
}

/// Print a command example in the "next steps" style
pub fn command(cmd: &str) {
    println!("    {}", style(format!("$ {}", cmd)).cyan());
}

/// Newline shorthand
pub fn br() {
    println!();
}

/// Custom help output — replaces clap's default
pub fn print_help() {
    banner();

    println!("  {}", style("USAGE:").bold().underlined());
    println!("    suprnova {}", style("<command> [options]").dim());
    br();

    println!("  {}", style("CREATE").bold().underlined());
    help_line("new [name]", "Create a new Suprnova project");
    help_line("serve", "Start dev servers (backend + frontend)");
    br();

    println!("  {}", style("GENERATE").bold().underlined());
    help_line("make:controller <name>", "Scaffold a new controller");
    help_line("make:action <name>", "Scaffold a new action");
    help_line("make:middleware <name>", "Scaffold a new middleware");
    help_line("make:migration <name>", "Scaffold a new migration");
    help_line("make:inertia <name>", "Scaffold an Inertia page");
    help_line("make:error <name>", "Scaffold a domain error");
    help_line("make:task <name>", "Scaffold a scheduled task");
    br();

    println!("  {}", style("DATABASE").bold().underlined());
    help_line("migrate", "Run pending migrations");
    help_line("migrate:status", "Show migration status");
    help_line("migrate:rollback", "Rollback last migration(s)");
    help_line("migrate:fresh", "Drop all tables & re-migrate");
    help_line("db:sync", "Sync schema → entity files");
    br();

    println!("  {}", style("SCHEDULE").bold().underlined());
    help_line("schedule:run", "Run due tasks once");
    help_line("schedule:work", "Start scheduler daemon");
    help_line("schedule:list", "List registered tasks");
    br();

    println!("  {}", style("WORKFLOW").bold().underlined());
    help_line("workflow:work", "Start workflow worker");
    help_line("workflow:install", "Install workflow migrations");
    br();

    println!("  {}", style("SSR").bold().underlined());
    help_line("ssr:start", "Launch Inertia SSR worker (foreground)");
    help_line("ssr:check", "Verify SSR worker is reachable");
    br();

    println!("  {}", style("DEPLOY").bold().underlined());
    help_line("docker:init", "Generate production Dockerfile");
    help_line("docker:compose", "Generate docker-compose.yml");
    br();

    println!("  {}", style("OTHER").bold().underlined());
    help_line("generate-types", "Generate TS types from Rust structs");
    help_line("web:run", "Run web server (production)");
    br();

    hint("Run 'suprnova <command> --help' for details on a specific command.");
    br();
}

fn help_line(cmd: &str, desc: &str) {
    let pad = 30_usize.saturating_sub(cmd.len());
    println!(
        "    {}{}{}",
        style(cmd).cyan(),
        " ".repeat(pad),
        style(desc).dim(),
    );
}
