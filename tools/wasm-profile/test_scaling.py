import unittest

from scaling import module_variant, summarize, unsigned_leb


class ScalingTests(unittest.TestCase):
    def test_custom_sections_have_distinct_identities_without_changing_module_prefix(
        self,
    ):
        source = b"\0asm\x01\0\0\0"
        first, second = module_variant(source, 0), module_variant(source, 1)
        self.assertTrue(first.startswith(source + b"\0"))
        self.assertEqual(len(first), len(second))
        self.assertNotEqual(first, second)
        self.assertEqual(unsigned_leb(127), b"\x7f")
        self.assertEqual(unsigned_leb(128), b"\x80\x01")

    def test_missing_worker_phases_are_rejected(self):
        with self.assertRaises(ValueError):
            summarize([{"mode": "native", "count": 1, "trial": 0, "phase": "baseline"}])

    def test_cpu_and_memory_use_correct_units_and_baselines(self):
        def work(calls):
            return {
                "calls": calls,
                "samples_ns": [2000] * calls,
                "wall_ns": calls * 3000,
                "cpu": {"user_ns": calls * 1000, "system_ns": calls * 500},
            }

        phases = {
            "baseline": {"rss_bytes": 1024 * 1024},
            "loaded": {
                "rss_bytes": 2 * 1024 * 1024,
                "load": {
                    "wall_ns": 10_000_000,
                    "cpu": {"user_ns": 4_000_000, "system_ns": 1_000_000},
                },
            },
            "warmed": {"rss_bytes": 4 * 1024 * 1024, "parity": True},
            "runtime": {
                "rss_bytes": 4 * 1024 * 1024,
                "idle": {
                    "wall_ns": 200_000_000,
                    "cpu": {"user_ns": 100_000, "system_ns": 100_000},
                },
                "single": work(1000),
                "fanout": work(100),
            },
            "unloaded": {"rss_bytes": 2 * 1024 * 1024},
        }
        rows = [
            dict(mode="native", count=2, trial=0, phase=phase, **row)
            for phase, row in phases.items()
        ]
        result = summarize(rows)[0]
        self.assertEqual(result["incremental_rss_mib"], 3)
        self.assertEqual(result["load_cpu_ms"], 5)
        self.assertEqual(result["single"]["cpu_us_per_call"], 1.5)
        self.assertEqual(result["fanout"]["cpu_ms_per_sweep"], 0.003)
        self.assertEqual(result["idle_core_percent"], 0.1)


if __name__ == "__main__":
    unittest.main()
