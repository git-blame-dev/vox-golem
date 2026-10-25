import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location(
    "generate_model_catalog", Path(__file__).with_name("generate-model-catalog.py")
)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class KotlinQuoteTest(unittest.TestCase):
    def test_interpolation_quotes_backslashes_and_line_breaks(self):
        self.assertEqual(
            generator.kotlin_quote('Synthetic $value ${expression} "quote" \\path\nnext\r\t'),
            '"Synthetic \\$value \\${expression} \\"quote\\" \\\\path\\nnext\\r\\t"',
        )

    def test_form_feed_uses_kotlin_unicode_escape(self):
        self.assertEqual(generator.kotlin_quote("\f"), '"\\u000c"')
        self.assertEqual(generator.kotlin_quote("\\foo"), '"\\\\foo"')


if __name__ == "__main__":
    unittest.main()
