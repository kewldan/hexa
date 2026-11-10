//! Language server for Hexa structure templates (SCR-08): speaks LSP over
//! stdin/stdout.

fn main() {
    std::process::exit(hexa_template::lsp::run_stdio());
}
