#!/usr/bin/env python3
"""Bounded mixed-load verification against a disposable LOCAL server, never production.

Usage: python3 ops/subject-cache/verify_load.py backend/target/debug/weimo-douban-server
"""
import collections
import concurrent.futures
import json
import os
import pathlib
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


def percentile(values, fraction):
    return sorted(values)[min(len(values) - 1, int(len(values) * fraction))]


def main():
    binary = pathlib.Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="douban-cache-load-") as folder:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = "http://127.0.0.1:%d" % port
        env = dict(os.environ, SYNC_SERVER_BIND="127.0.0.1:%d" % port,
                   SYNC_SERVER_DATA_DIR=folder, SYNC_SERVER_TOKEN="local-load-test",
                   SYNC_SUBJECT_ENRICH="false", SYNC_SUBJECT_CACHE_SERVE="true",
                   SYNC_SUBJECT_CACHE_URL="", SYNC_RATE_PER_MINUTE="300")
        with open(pathlib.Path(folder) / "server.log", "w") as log:
            process = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
            try:
                for _ in range(100):
                    try:
                        urllib.request.urlopen(base + "/healthz", timeout=.2).close()
                        break
                    except Exception:
                        if process.poll() is not None:
                            raise RuntimeError("local test server exited")
                        time.sleep(.1)
                else:
                    raise RuntimeError("local test server did not start")
                db = sqlite3.connect(str(pathlib.Path(folder) / "douban_export.db"))
                now = int(time.time() * 1000)
                db.execute("INSERT INTO dimensions(uid,category,dim_status,state,fetched_ms) VALUES('load','movie','collect','ok',?)", (now,))
                meta = json.dumps({"directors": ["测试导演"], "intro": "本地容量测试。" * 75}, ensure_ascii=False)
                for i in range(1, 2001):
                    db.execute("INSERT INTO records(uid,category,dim_status,subject_id,title) VALUES('load','movie','collect',?,?)", (str(i), "测试条目 %d" % i))
                    db.execute("INSERT INTO subjects(category,subject_id,title,meta_json,fetched_ms) VALUES('movie',?,?,?,?)", (str(i), "测试条目 %d" % i, meta, now))
                db.commit()
                db.close()

                def request(path, payload=None):
                    headers = {"Content-Type": "application/json"}
                    if payload is None:
                        headers["Authorization"] = "Bearer local-load-test"
                    req = urllib.request.Request(base + path, data=payload, headers=headers)
                    started = time.perf_counter()
                    try:
                        with urllib.request.urlopen(req, timeout=5) as response:
                            response.read()
                            status = response.status
                    except urllib.error.HTTPError as error:
                        error.read()
                        status = error.code
                    return status, (time.perf_counter() - started) * 1000

                path = "/api/v1/users/load/marks?category=movie&status=collect&page_size=20&expand=subject"
                baseline = [request(path) for _ in range(30)]

                def app():
                    results = []
                    for batch in range(20):
                        payload = json.dumps({"items": [{"category": "movie", "subject_id": str(i)}
                                                        for i in range(batch * 100 + 1, batch * 100 + 101)]}).encode()
                        results.append(request("/api/v1/subjects/batch", payload)[0])
                    return results

                with concurrent.futures.ThreadPoolExecutor(max_workers=31) as pool:
                    apps = [pool.submit(app) for _ in range(30)]
                    web = pool.submit(lambda: [request(path) for _ in range(100)])
                    statuses = collections.Counter(status for future in apps for status in future.result())
                    mixed = web.result()
                baseline_p95 = percentile([ms for _, ms in baseline], .95)
                mixed_p95 = percentile([ms for _, ms in mixed], .95)
                result = {"clients": 30, "subjects_per_client": 2000,
                          "cache_requests": 600, "cache_statuses": dict(statuses),
                          "website_requests": len(mixed),
                          "website_statuses": dict(collections.Counter(code for code, _ in mixed)),
                          "baseline_web_p95_ms": round(baseline_p95, 2),
                          "mixed_web_p95_ms": round(mixed_p95, 2),
                          "web_p95_increase_ms": round(mixed_p95 - baseline_p95, 2)}
                print(json.dumps(result, indent=2))
                assert all(code == 200 for code, _ in baseline + mixed), result
                assert mixed_p95 - baseline_p95 <= 100, result
                assert set(statuses).issubset({200, 429, 503}), result
                assert statuses[200] > 0, "at least one real cache response must succeed"
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    main()
