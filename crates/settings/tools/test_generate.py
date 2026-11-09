#!/usr/bin/env python3
"""Regression tests for gettext parsing and source-specific extraction helpers."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('generate', Path(__file__).with_name('generate.py'))
generate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generate)


class GettextTests(unittest.TestCase):
    def test_layout_reused_line_paths_follow_source_order(self):
        bodies = {
            'TabPrint::build': '''void TabPrint::build() {
                add_options_page("Quality", "");
                auto* group = page->new_optgroup("Temperature", "");
                Option line;
                line.label_path = "temperature#first";
                line.append_option("first_temperature");
                group->append_line(line);
                line.label_path = "temperature#second";
                line.append_option("second_temperature");
                group->append_line(line);
                line.append_option("no_path_temperature");
                group->append_line(line);
            }''',
            'TabFilament::build': 'void TabFilament::build() { add_options_page("Filament", ""); }',
            'TabFilament::add_filament_overrides_page': '',
            'TabPrinter::build_fff': '''void TabPrinter::build_fff() {
                add_options_page("Basic information", "");
                add_options_page("Machine G-code", "");
                add_options_page("Multimaterial", "");
                add_options_page("Extruder", "");
            }''',
            'TabPrinter::build_kinematics_page': 'void TabPrinter::build_kinematics_page() { add_options_page("Motion ability", ""); }',
            'TabPrinter::build_unregular_pages': 'void TabPrinter::build_unregular_pages() { int n_before_extruders = 0; int n_after_single_extruder_MM = 0; }',
        }

        def fixture_function(_source, name):
            return bodies[name]

        with patch.object(generate, 'function', side_effect=fixture_function):
            layout, warnings, doc_urls = generate.extract_layout('', '', orca=True)

        self.assertEqual(warnings, [])
        quality = next(page for page in layout['process'] if page['title'] == 'Quality')
        rows = quality['groups'][0]['lines']
        self.assertEqual([row['options'] for row in rows], [
            ['first_temperature'], ['second_temperature'], ['no_path_temperature']
        ])
        self.assertEqual(doc_urls, {
            'first_temperature': 'https://www.orcaslicer.com/wiki/temperature#first',
            'second_temperature': 'https://www.orcaslicer.com/wiki/temperature#second',
        })

    def test_context_multiline_escapes_and_unapproved_entries(self):
        catalog = generate.parse_po(r'''
msgid ""
msgstr "Language: uk\n"

msgctxt "verb"
msgid "Print"
msgstr "Друкувати"

msgid "Print"
msgstr "Друк"

msgid "Line\n"
"\"two\" \\ path"
msgstr "Рядок\n"
"\"два\" \\ шлях"

#, c-format, fuzzy
msgid "Unreviewed"
msgstr "Неперевірений"

msgid "Empty"
msgstr ""

#~ msgid "Obsolete"
#~ msgstr "Старе"

msgid "item"
msgid_plural "items"
msgstr[0] "елемент"
msgstr[1] "елементи"
''')
        self.assertEqual(catalog[('verb', 'Print')], 'Друкувати')
        self.assertEqual(catalog[('', 'Print')], 'Друк')
        self.assertEqual(catalog[('', 'Line\n"two" \\ path')], 'Рядок\n"два" \\ шлях')
        self.assertEqual(len(catalog), 3)

    def test_context_lookup_does_not_use_other_context(self):
        option = dict(label='Print', full_label='', tooltip='', sidetext='', category='',
                      enum_labels=['Print', 'Missing'], _contexts={'label': 'verb'})
        generate.localize({'key': option}, {}, {('verb', 'Print'): 'Друкувати', ('', 'Print'): 'Друк'})
        self.assertEqual(option['uk']['label'], 'Друкувати')
        self.assertEqual(option['uk']['enum_labels'], ['Друк', ''])

    def test_entries_without_blank_separator(self):
        catalog = generate.parse_po('msgid "First"\nmsgstr "Перший"\n'
            '#, fuzzy\nmsgid "Second"\nmsgstr "Другий"\n'
            'msgctxt "noun"\nmsgid "Third"\nmsgstr "Третій"\n')
        self.assertEqual(catalog, {('', 'First'): 'Перший', ('noun', 'Third'): 'Третій'})

    def test_compile_disabled_blocks_are_excluded(self):
        source = '#if 0\ndisabled\n#if OTHER\nnested\n#endif\n#else\nactive\n#endif\n#if 1\nfirst\n#else\nsecond\n#endif\n'
        self.assertEqual(generate.clean(source), 'active\nfirst\n')

    def test_documentation_url_formatting(self):
        # Bambu anchor normalization (lowercased, spaces to hyphens)
        bambu_url = generate.format_documentation_url('fill-patterns#Infill of the top surface and bottom surface', orca=False)
        self.assertEqual(bambu_url, 'https://wiki.bambulab.com/en/software/bambu-studio/fill-patterns#infill-of-the-top-surface-and-bottom-surface')

        # Bambu simple path without anchor
        bambu_simple = generate.format_documentation_url('wall-generator', orca=False)
        self.assertEqual(bambu_simple, 'https://wiki.bambulab.com/en/software/bambu-studio/wall-generator')

        # Orca wiki URL
        orca_url = generate.format_documentation_url('printer_basic_information_printable_space#z-offset', orca=True)
        self.assertEqual(orca_url, 'https://www.orcaslicer.com/wiki/printer_basic_information_printable_space#z-offset')

        # Empty / None path
        self.assertIsNone(generate.format_documentation_url('', orca=False))
        self.assertIsNone(generate.format_documentation_url(None, orca=True))


if __name__ == '__main__':
    unittest.main()
