"""Private typing helpers must not become public runtime API documentation."""

import ast
import unittest

from docs.generate_python_api import _extract_classes_and_functions, generate


class PythonApiTests(unittest.TestCase):
    def test_private_classes_and_functions_are_excluded(self):
        tree = ast.parse(
            "class _TypingOnly: pass\n"
            "class App: pass\n"
            "def _helper(): pass\n"
            "def input_sim(): pass\n"
        )
        classes, functions = _extract_classes_and_functions(tree)
        self.assertEqual([node.name for node in classes], ["App"])
        self.assertEqual([node.name for node in functions], ["input_sim"])

    def test_real_stub_typing_dictionary_is_not_rendered_as_a_runtime_class(self):
        output = generate()
        self.assertNotIn("### `_PointerTargetDiagnosis`", output)
        self.assertIn("### `InputSim`", output)
        self.assertIn("### `App`", output)


if __name__ == "__main__":
    unittest.main()
