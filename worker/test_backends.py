import unittest
from backends import vl_options, load_backend

class BackendOptionsTest(unittest.TestCase):
    def test_defaults(self):
        self.assertEqual(vl_options({}), (1003520, 512))

    def test_limits_protect_vram_and_generation(self):
        for request in ({"max_pixels": 0}, {"max_pixels": 99999999},
                        {"max_new_tokens": 0}, {"max_new_tokens": 100000}):
            with self.assertRaises(ValueError):
                vl_options(request)

    def test_unknown_model_is_not_silently_replaced(self):
        with self.assertRaisesRegex(ValueError, "Unsupported"):
            load_backend("nonexistent", "unused", "cpu", {})

if __name__ == "__main__":
    unittest.main()
