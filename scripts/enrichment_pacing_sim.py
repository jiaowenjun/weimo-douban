#!/usr/bin/env python3
"""Offline sensitivity experiment, NOT an estimate of Douban's quota.

Run: python3 scripts/enrichment_pacing_sim.py --subjects 1983
No network requests. Same workload and random seed for each pacing floor.
Two deliberately different token-bucket hypotheses expose quota sensitivity.
"""
import argparse
import json
import random


def simulate(subjects, floor, refill_seconds, capacity, seed):
    rng = random.Random(seed)
    now = 0.0
    tokens = float(capacity)
    last = 0.0
    interval = floor
    successes = blocked = 0
    while successes < subjects:
        tokens = min(capacity, tokens + (now - last) / refill_seconds)
        last = now
        if tokens >= 1:
            tokens -= 1
            successes += 1
            interval = max(floor, interval * 0.9)
            now += interval + rng.random()
        else:
            blocked += 1
            interval = min(90, interval * 3)
            now += 300 + rng.random() * 30
    return {
        'floor_seconds': floor,
        'successes': successes,
        'blocked_attempts': blocked,
        'simulated_hours': round(now / 3600, 2),
        'successes_per_hour': round(successes * 3600 / now, 2),
        'blocked_percent': round(100 * blocked / (successes + blocked), 2),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--subjects', type=int, default=1983)
    parser.add_argument('--seed', type=int, default=20261102)
    args = parser.parse_args()
    if args.subjects < 1:
        parser.error('--subjects must be positive')
    models = [
        ('hypothesis_refill_30s_burst_4', 30, 4),
        ('hypothesis_refill_90s_burst_4', 90, 4),
    ]
    output = {
        'scope': 'Synthetic token-bucket sensitivity analysis; no live traffic or inferred Douban quota.',
        'limitations': 'No restarts, competing dimensions, response latency or finite retry budget; isolates pacing and cooldown only.',
        'subjects': args.subjects,
        'seed': args.seed,
        'models': [{'name': name, 'refill_seconds': refill, 'capacity': capacity,
                    'results': [simulate(args.subjects, floor, refill, capacity, args.seed)
                                for floor in (3, 45, 60)]}
                   for name, refill, capacity in models],
    }
    print(json.dumps(output, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
