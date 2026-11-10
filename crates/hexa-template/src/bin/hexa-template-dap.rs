//! Debug adapter for Hexa structure templates (STRC-18): speaks DAP over
//! stdin/stdout. Launch arguments: `template`, `data`, optional `rootType`,
//! `offset` and `stopOnEntry`.

fn main() {
    std::process::exit(hexa_template::dap::run_stdio());
}
