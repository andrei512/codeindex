import unittest
from subject import transform


class TransformTest(unittest.TestCase):
    def test_transform(self):
        cases = [
                (-2, 2), (-1, 1), (0, 0), (1, 2), (2, 4), (3, 6)
            ]
        for value, expected in cases:
            self.assertEqual(transform(value), expected)
