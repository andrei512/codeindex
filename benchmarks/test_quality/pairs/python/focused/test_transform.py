import unittest
from subject import transform


class TransformTest(unittest.TestCase):
    def test_negative(self):
        self.assertEqual(transform(-2), 2)
        self.assertEqual(transform(-1), 1)

    def test_nonnegative(self):
        self.assertEqual(transform(0), 0)
        self.assertEqual(transform(1), 2)
        self.assertEqual(transform(2), 4)
        self.assertEqual(transform(3), 6)
