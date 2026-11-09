# Interface languages

Editor settings (Ctrl+,) → Interface language switches Ukrainian and English
immediately. The choice is saved in `preferences.json`. Switching refreshes
preset controls without replacing the editable preset or discarding overrides.
Bambu/Orca labels, enum values and help use the selected English/Ukrainian
metadata already shipped in the settings catalogs.

Application text lives in `crates/app/locales/en.json`: exact Ukrainian source
keys and English values. `i18n::tr` is for static UI text; `tr_format!` retains
Rust formatting and named/positional placeholders for dynamic UI templates.
Do not translate imported model names, profile IDs, filesystem paths or G-code.
Raw engine/SDK diagnostics can remain in their original language.

## Add a language pack

Create `$XDG_CONFIG_HOME/slicenv/locales/es.json` (normally
`~/.config/slicenv/locales/es.json`):

```json
{
  "_language_name": "Español",
  "Головна": "Inicio",
  "Підготовка": "Preparar",
  "Перегляд": "Vista previa",
  "Проект": "Proyecto",
  "Готово": "Listo"
}
```

Open preferences again: the pack appears alongside Ukrainian and English.
Translate the remaining keys from `en.json` for full coverage. Missing custom
translations fall back to English, then the source text. Other languages use
English engine metadata until their own translated settings catalogs are added.
Invalid/non-string JSON packs are omitted; file names accept only letters,
digits, `_` and `-`. Pack names and values are plain text.

Do not change placeholders (`{n}`, `{:.2}`, `{}`) or their formatting modifiers.
Adjacent placeholders without a literal separator are left in the source
rendering rather than guessing how to split user values. Templates with repeated
fields rendered with different precision likewise fall back to their intact
rendering. Icons and language-neutral diagrams are shared by all languages.

Audit literal-key coverage:

```sh
python3 crates/app/tools/check_translations.py
```

This checker identifies missing catalog entries; it does not prove that every
runtime display site is wired or that external errors are translated. Native
language replay covers live switching, dirty preset preservation and restart.

## Acceptance (2026-10-30)

519 app catalog keys; source checker and placeholder checks pass. Native replay
`crates/app/verification/languages.py` verified UK → EN → UK → partial Spanish
with English fallback, preservation of a dirty layer-height override and saved
language after restart. The fake Spanish pack is a test fixture, not a complete
shipped translation. Receipts/screenshots: `/tmp/bambu-languages-final`.

The printer panel was visually checked in English on a fake printing/idle A1,
including all four tabs and the guarded home dialog. Imported names remain in
their original language. Raw formatted engine/SDK/control diagnostics can retain
their source language; catalog presence alone does not translate those paths.
