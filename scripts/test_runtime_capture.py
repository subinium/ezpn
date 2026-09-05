import random
import unittest

from runtime_driver import TailBuffer


class CaptureTests(unittest.TestCase):
    def test_wrap_and_oversized_chunk_keep_the_exact_tail(self):
        capture = TailBuffer(8)
        for chunk, expected in [(b"abc", b"abc"), (b"defgh", b"abcdefgh"), (b"ij", b"cdefghij"),
                                (b"0123456789", b"23456789"), (b"", b"23456789"), (b"xy", b"456789xy")]:
            capture.append(chunk)
            self.assertEqual(capture.snapshot(), expected)

    def test_fragmented_capture_matches_a_bounded_reference(self):
        generator = random.Random(42)
        capture = TailBuffer(257)
        expected = b""
        for _ in range(1000):
            chunk = bytes(generator.randrange(256) for _ in range(generator.randrange(600)))
            expected = (expected + chunk)[-257:]
            capture.append(chunk)
            self.assertEqual(capture.snapshot(), expected)

    def test_zero_capacity_is_rejected(self):
        with self.assertRaises(ValueError):
            TailBuffer(0)


if __name__ == "__main__":
    unittest.main()
