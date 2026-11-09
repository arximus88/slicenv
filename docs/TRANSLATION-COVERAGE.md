# English translation coverage

The English catalog is [`crates/app/locales/en.json`](../crates/app/locales/en.json), a flat UTF-8 JSON object keyed by exact Ukrainian source strings. Keep Rust formatting placeholders and exception markers intact in translated values; format strings are looked up by their source template. The catalog includes the language chooser labels `Мова`, `Українська`, and `Мова інтерфейсу`.

## Audited source scope

Run `python3 crates/app/tools/check_translations.py` from any directory to check literal source-key presence. It recursively scans Rust files under `crates/app/src`, including `device.rs` and nested `device/**`, except `i18n.rs`. It also scans `crates/gcode/src/model.rs`, `crates/gcode/src/view.rs`, `crates/cloud/src/status.rs`, `crates/cloud/src/control.rs`, and `crates/cloud/src/cover.rs`. The scan masks comments and `#[cfg(test)]` modules. Logger payloads, fake-device labels, and interpolation fragments are excluded because they are not standalone translated display strings.

The cloud control and cover validation templates are included as source keys, with format placeholders retained. Presence in the catalog does not itself translate an error: the current cloud control and cover paths return raw formatted `anyhow` errors and do not pass them through the app locale lookup. Therefore this audit does not claim full runtime coverage for those errors.

## Limits

This is a source-key presence check, not proof that every string shown by the application is localized. It approximates ordinary Rust double-quoted literals and does not inspect raw or multiline strings, generated text, non-Rust assets, dynamically composed labels, or strings outside the listed files. English source strings need no separate entry. Keep this document aligned with the checker scope when adding translation sources.
