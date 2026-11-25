import unittest
from subject import transform


class TransformTest(unittest.TestCase):
    def test_transform(self):
        for value in [-2, -1, 0, 1, 2, 3]:
            if value < 0:
                self.assertEqual(transform(value), -value)
            else:
                self.assertEqual(transform(value), value * 2)
