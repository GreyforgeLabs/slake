//! Deprecated alias for `slake`, kept for one release so existing cron lines and
//! scripts keep working. It prints a one-line note to stderr and then behaves
//! exactly like `slake`; stdout and exit codes are unchanged.

fn main() {
    eprintln!("{}", slake::DEPRECATED_ALIAS_NOTE);
    slake::main_entry()
}
