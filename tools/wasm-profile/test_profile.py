import unittest
from profile import make_gate, percentile, relative_latency, stress_failures


class GateTests(unittest.TestCase):
    def test_ratio_threshold_and_timer_floor(self):
        self.assertTrue(
            make_gate("ratio", relative_latency(100, 1500), 1.5, "ratio")["passed"]
        )
        self.assertFalse(
            make_gate("ratio", relative_latency(1000, 1501), 1.5, "ratio")["passed"]
        )
        self.assertFalse(
            make_gate("ratio", relative_latency(10000, 20001), 2, "ratio")["passed"]
        )

    def test_percentile_uses_individual_samples(self):
        self.assertEqual(percentile([1, 2, 3, 4, 100], 0.99), 100)

    def test_missing_stress_data_is_an_error(self):
        with self.assertRaises(ValueError):
            stress_failures([])

    def test_traps_fail_gate(self):
        queries = [
            "print",
            "pritn",
            "pront",
            "compleiton",
            "Completion",
            "界接",
            "zzzz",
            "",
            "create_workspace",
        ]
        samples = [{"query": q, "ok": True} for q in queries]
        self.assertEqual(stress_failures(samples), 0)
        samples[0]["ok"] = False
        self.assertEqual(stress_failures(samples), 1)


if __name__ == "__main__":
    unittest.main()
