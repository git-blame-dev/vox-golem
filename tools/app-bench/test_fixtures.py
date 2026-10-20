import hashlib
import http.client
import json
from pathlib import Path
import struct
import tempfile
import time
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from fixtures import CASES, FakeServices, LONG_ANSWER, MAX_BODY, MODEL, URLS, generate_fixtures, matches_transcript


def request(endpoint, body=None, method="POST", headers=None):
    url = urlsplit(endpoint)
    connection = http.client.HTTPConnection(url.hostname, url.port, timeout=4)
    try:
        connection.request(method, url.path, json.dumps(body).encode() if body is not None else None,
                           headers or {"Content-Type": "application/json"})
        response = connection.getresponse()
        return response.status, response.read()
    finally:
        connection.close()


def model_request(case):
    return {"model": MODEL, "reasoning": {"effort": "none"}, "input": [
        {"role": "user", "content": [{"type": "input_text", "text": CASES[case]}]}]}


def events(data):
    return [json.loads(line[6:]) for line in data.decode().splitlines() if line.startswith("data: ")]


def tool_output(call_id, kind, urls):
    # search.rs serializes ToolResult (core/search.rs) under this wrapper, not MCP.
    return {"type": "function_call_output", "call_id": call_id, "output": json.dumps({
        "untrusted_web_evidence": {
            "sources": [{"id": "synthetic-source-" + str(i), "url": url,
                         "title": "Synthetic documentation", "provider": "exa", "kind": kind,
                         "text": "Synthetic readable documentation evidence.", "retrieved_unix_ms": 1,
                         "publisher_date": None, "truncated": False} for i, url in enumerate(urls)],
            "failures": [], "fallback_used": False, "evidence_unavailable": False,
            "partial": False, "raw_text": None}})}


class FixtureTests(unittest.TestCase):
    def open_stream(self, service):
        url = urlsplit(service.generation_endpoint)
        connection = http.client.HTTPConnection(url.hostname, url.port, timeout=3)
        self.addCleanup(connection.close)
        connection.request("POST", url.path, json.dumps(model_request("quick")))
        response = connection.getresponse()
        self.assertEqual(events(response.readline())[0]["delta"], "4")
        return response

    def test_gated_stream_release_and_quiescence(self):
        with FakeServices(gated_stream=True) as service:
            response = self.open_stream(service)
            self.assertFalse(service.wait_quiescent(timeout=0.02))
            self.assertFalse(service.snapshot()["terminal"])
            self.assertTrue(service.snapshot()["first_delta_sent"])
            with self.assertRaisesRegex(RuntimeError, "generation_active"):
                service.expected_case = "quick"
            service.release_stream()
            self.assertEqual(events(response.read())[-1]["type"], "response.completed")
            self.assertTrue(service.wait_quiescent())
            self.assertTrue(service.snapshot()["terminal"])
            self.assertFalse(service.snapshot()["stream_gate_timed_out"])
            service.expected_case = "quick"
            self.assertFalse(service.snapshot()["first_delta_sent"])
            self.assertFalse(service._stream_gate.is_set())

    def test_gate_timeout_finishes_but_fails(self):
        with patch("fixtures.STREAM_GATE_TIMEOUT_SECONDS", 0.1):
            with FakeServices(gated_stream=True) as service:
                response = self.open_stream(service)
                self.assertEqual(events(response.read())[-1]["type"], "response.completed")
                self.assertTrue(service.wait_quiescent())
                stats = service.snapshot()
                self.assertTrue(stats["stream_gate_timed_out"])
                self.assertGreaterEqual(stats["stream_gate_wait_ms"], 80)
                self.assertFalse(stats["terminal"])
                self.assertEqual(stats["failures"], 1)

    def test_shutdown_releases_gate(self):
        start = time.monotonic()
        with FakeServices(gated_stream=True) as service:
            response = self.open_stream(service)
        self.assertLess(time.monotonic() - start, 2)
        self.assertTrue(service.wait_quiescent())
        self.assertEqual(events(response.read())[-1]["type"], "response.completed")

    def test_exact_canonical_history(self):
        turns = [{"user": {"content": {"text": "Synthetic old question"}},
                  "answer": {"content": {"text": "Synthetic answer\n\nSources: synthetic footer"}}}]
        history = [{"role": "user", "content": [{"type": "input_text", "text": turns[0]["user"]["content"]["text"]}]},
                   {"role": "assistant", "content": [{"type": "output_text", "text": turns[0]["answer"]["content"]["text"]}]}]
        variants = [history, [], history[::-1], history + history,
                    [history[0], {"role": "assistant", "content": "Synthetic answer"}]]
        with FakeServices() as service:
            for index, observed in enumerate(variants):
                with self.subTest(index=index):
                    self.assertTrue(service.wait_quiescent())
                    service.expected_case = "quick"
                    service.expect_history(turns)
                    body = model_request("quick")
                    body["input"][:0] = [{"role": "system", "content": "Synthetic instructions"}] + observed
                    self.assertEqual(request(service.generation_endpoint, body)[0], 200 if index == 0 else 400)
                    self.assertEqual(service.snapshot()["history_matches"], index == 0)
                    self.assertEqual(service.snapshot()["history_turns_expected"], 1)
            sourced_turns = [{"user": {"content": {"text": "Synthetic sourced question"}},
                              "answer": {"content": {"text": "Synthetic plain answer"}},
                              "sources": [{"url": "https://benchmark.example.test/once-lock", "title": "Synthetic Doc"}]}]
            for valid_bullet in (True, False):
                self.assertTrue(service.wait_quiescent())
                service.expected_case = "quick"
                service.expect_history(sourced_turns)
                body = model_request("quick")
                footer = "\n\nSources:\n- https://benchmark.example.test/once-lock" if valid_bullet else "\n\nSources:\nhttps://benchmark.example.test/once-lock"
                body["input"][:0] = [
                    {"role": "user", "content": [{"type": "input_text", "text": "Synthetic sourced question"}]},
                    {"role": "assistant", "content": [{"type": "output_text", "text": "Synthetic plain answer" + footer}]},
                ]
                self.assertEqual(request(service.generation_endpoint, body)[0], 200 if valid_bullet else 400)
                self.assertEqual(service.snapshot()["history_matches"], valid_bullet)

    def test_history_is_checked_on_every_research_round(self):
        turns = [{"user": {"content": {"text": "Synthetic prior user"}},
                  "answer": {"content": {"text": "Synthetic prior answer"}}}]
        with FakeServices() as service:
            for tamper in (False, True):
                self.assertTrue(service.wait_quiescent())
                service.expected_case = "short_research"
                service.expect_history(turns)
                body = model_request("short_research")
                body["input"][:0] = [{"role": "user", "content": "Synthetic prior user"},
                                     {"role": "assistant", "content": "Synthetic prior answer"}]
                self.assertEqual(request(service.generation_endpoint, body)[0], 200)
                body["input"].append(tool_output("synthetic-search", "search_excerpt", URLS))
                self.assertEqual(request(service.generation_endpoint, body)[0], 200)
                body["input"].append(tool_output("synthetic-fetch", "page_read", URLS[:1]))
                if tamper:
                    body["input"][1]["content"] = "Synthetic changed answer"
                self.assertEqual(request(service.generation_endpoint, body)[0], 400 if tamper else 200)
                self.assertTrue(service.wait_quiescent())
                self.assertEqual(service.snapshot()["model_rounds"], 2 if tamper else 3)
                self.assertEqual(service.snapshot()["history_matches"], not tamper)

    def test_current_client_offer_negotiates_the_supported_initialized_protocol(self):
        with FakeServices() as service:
            status, raw = request(service.mcp_endpoint, {'jsonrpc': '2.0', 'id': 1,
                'method': 'initialize', 'params': {'protocolVersion': '2026-07-28',
                    'capabilities': {}, 'clientInfo': {'name': 'synthetic-client', 'version': '1'}}})
            self.assertEqual(status, 200)
            self.assertEqual(json.loads(raw)['result']['protocolVersion'], '2025-11-25')
            self.assertEqual(service.snapshot()['initializations'], 1)

    def test_transcripts(self):
        for case, text in CASES.items():
            self.assertTrue(matches_transcript(case, text))
            self.assertFalse(matches_transcript(case, "Synthetic unrelated request"))
        self.assertTrue(matches_transcript("quick", "What is 17 plus 25 reply only with the number"))
        self.assertTrue(matches_transcript("short_research", CASES["short_research"].replace("Once Lock", "OnceLock").replace("release", "version")))
        self.assertTrue(matches_transcript("short_research", CASES["short_research"].replace("Once Lock", "once locked").replace("sourced", "source")))
        self.assertTrue(matches_transcript("short_research", "What Rust release introduced one's luck? Read the official documentation and give a brief sourced answer."))
        self.assertTrue(matches_transcript("long_research", CASES["long_research"].replace("Lazy Lock", "LazyLock").replace("behavior", "behaviour")))
        self.assertTrue(matches_transcript("long_research", CASES["long_research"].replace("Once Lock", "once locked").replace("Lazy Lock", "lazy locked")))
        self.assertTrue(matches_transcript("long_research", "Research One's lock and lazy lock in Rust. Read the official documentation and compare their release versions, initialization timing, panic behavior, and ability to reset. Explain the important differences in several short paragraphs with sources."))
        self.assertFalse(matches_transcript("quick", "What is 18 plus 25 reply only with the number"))
        self.assertFalse(matches_transcript("unknown", ""))

    def test_audio_boundaries(self):
        with tempfile.TemporaryDirectory(prefix="synthetic-fixture-", dir="/tmp/opencode") as directory:
            metadata = generate_fixtures(directory)
            for case, item in metadata.items():
                raw = (Path(directory) / f"{case}.pcm").read_bytes()
                samples = struct.unpack("<" + "f" * (len(raw) // 4), raw)
                self.assertEqual(item["sha256"], hashlib.sha256(raw).hexdigest())
                self.assertEqual(item["byte_count"], len(raw))
                self.assertEqual(item["sample_count"], len(samples))
                self.assertEqual(item["question_end_sample"], len(samples))
                self.assertEqual(samples[:32000], (0.0,) * 32000)
                self.assertEqual(item["question_start_sample"] - item["wake_end_sample"], 5600)
                self.assertNotEqual(samples[-1], 0)
                self.assertTrue(all(-1 <= sample <= 1 for sample in samples))

    def test_quick_and_privacy_and_shutdown(self):
        with FakeServices() as service:
            status, data = request(service.generation_endpoint, model_request("quick"))
            self.assertEqual(status, 200)
            wire = events(data)
            self.assertEqual(wire[0]["delta"], "4")
            self.assertEqual(wire[-1]["response"]["output"][0]["content"][0]["text"], "42")
            self.assertTrue(service.snapshot()["terminal"])
            snapshot = service.snapshot()
            self.assertTrue(all(isinstance(value, (int, float, bool)) for value in snapshot.values()))
            serialized = json.dumps(snapshot)
            for private in ("127.0.0.1", "http", CASES["quick"], "synthetic-answer"):
                self.assertNotIn(private, serialized)
        self.assertFalse(service._thread.is_alive())

    def test_paced_final_stream(self):
        with FakeServices() as service:
            url = urlsplit(service.generation_endpoint)
            connection = http.client.HTTPConnection(url.hostname, url.port, timeout=3)
            try:
                connection.request("POST", url.path, json.dumps(model_request("quick")))
                response = connection.getresponse()
                first = response.readline()
                first_seen = time.monotonic()
                self.assertEqual(events(first)[0]["delta"], "4")
                self.assertFalse(service.snapshot()["terminal"])
                remaining = response.read()
                self.assertGreaterEqual(time.monotonic() - first_seen, 0.30)
                self.assertEqual(events(remaining)[0]["delta"], "2")
                self.assertEqual(events(remaining)[-1]["type"], "response.completed")
                self.assertEqual(service.snapshot()["stream_delay_ms"], 400)
            finally:
                connection.close()

    def test_failed_or_incomplete_evidence_never_settles(self):
        variants = {
            "failed": {"error": "transport"},
            "missing": {},
            "partial": {"untrusted_web_evidence": {"sources": [], "failures": [], "partial": True, "evidence_unavailable": True}},
            "one_page": json.loads(tool_output("synthetic-fetch", "page_read", URLS[:1])["output"]),
            "search_only": json.loads(tool_output("synthetic-fetch", "search_excerpt", URLS)["output"]),
            "unreadable": json.loads(tool_output("synthetic-fetch", "page_read", URLS)["output"]),
            "partial_with_sources": json.loads(tool_output("synthetic-fetch", "page_read", URLS)["output"]),
            "page_failure": json.loads(tool_output("synthetic-fetch", "page_read", URLS)["output"]),
        }
        variants["unreadable"]["untrusted_web_evidence"]["sources"][0]["text"] = " \n\t"
        variants["partial_with_sources"]["untrusted_web_evidence"]["partial"] = True
        variants["page_failure"]["untrusted_web_evidence"]["failures"] = [{"url": URLS[0], "failure": "transport"}]
        with FakeServices() as service:
            for label, payload in variants.items():
                with self.subTest(label=label):
                    self.assertTrue(service.wait_quiescent())
                    service.expected_case = "long_research"
                    body = model_request("long_research")
                    self.assertEqual(request(service.generation_endpoint, body)[0], 200)
                    body["input"].append(tool_output("synthetic-search", "search_excerpt", URLS))
                    self.assertEqual(request(service.generation_endpoint, body)[0], 200)
                    body["input"].append({"type": "function_call_output", "call_id": "synthetic-fetch", "output": json.dumps(payload)})
                    self.assertEqual(request(service.generation_endpoint, body)[0], 400)
                    self.assertFalse(service.snapshot()["terminal"])
            self.assertTrue(service.wait_quiescent())
            service.expected_case = "short_research"
            body = model_request("short_research")
            self.assertEqual(request(service.generation_endpoint, body)[0], 200)
            body["input"].append(tool_output("synthetic-search", "page_read", URLS))
            self.assertEqual(request(service.generation_endpoint, body)[0], 400)

    def test_wrong_ids_and_unexpected_dispatch(self):
        with FakeServices() as service:
            for case in CASES:
                with self.subTest(case=case):
                    self.assertTrue(service.wait_quiescent())
                    service.expected_case = case
                    body = model_request(case)
                    self.assertEqual(request(service.generation_endpoint, body)[0], 200)
                    # A repeat of the first request is not a continuation or a new flow.
                    self.assertEqual(request(service.generation_endpoint, body)[0], 400)
                    self.assertFalse(service.snapshot()["terminal"])
                    self.assertGreater(service.snapshot()["generation_requests"], service.snapshot()["model_rounds"])
            self.assertTrue(service.wait_quiescent())
            service.expected_case = "short_research"
            body = model_request("short_research")
            self.assertEqual(request(service.generation_endpoint, body)[0], 200)
            body["input"].append(tool_output("synthetic-wrong-id", "search_excerpt", URLS))
            self.assertEqual(request(service.generation_endpoint, body)[0], 400)
            self.assertFalse(service.snapshot()["terminal"])

    def test_invalid_requests(self):
        with FakeServices() as service:
            for change in ("model", "prompt", "effort"):
                body = model_request("quick")
                if change == "model":
                    body["model"] = "synthetic-wrong-model"
                elif change == "effort":
                    body["reasoning"]["effort"] = "high"
                else:
                    body["input"][0]["content"][0]["text"] = "Synthetic private mismatch"
                status, data = request(service.generation_endpoint, body)
                self.assertEqual(status, 400)
                self.assertNotIn(b"private", data)
                self.assertFalse(service.snapshot()["terminal"])
            status, _ = request(service.generation_endpoint, {}, headers={"Content-Length": str(MAX_BODY + 1)})
            self.assertEqual(status, 413)
            self.assertEqual(request(service.mcp_endpoint, method="GET")[0], 405)
            self.assertEqual(request(service.mcp_endpoint, method="DELETE")[0], 200)

    def test_mcp_and_research_continuation(self):
        with FakeServices() as service:
            def rpc(method, params=None):
                status, data = request(service.mcp_endpoint, {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}})
                self.assertEqual(status, 200)
                return json.loads(data)["result"]

            result = rpc("initialize", {"protocolVersion": "2025-03-26"})
            self.assertEqual(result["protocolVersion"], "2025-03-26")
            self.assertEqual(request(service.mcp_endpoint, {"jsonrpc": "2.0", "method": "notifications/initialized"})[0], 202)
            tools = rpc("tools/list")["tools"]
            self.assertEqual({tool["name"] for tool in tools}, {"web_search_exa", "web_fetch_exa", "web_search", "web_fetch"})
            for tool in tools:
                self.assertEqual(tool["inputSchema"]["type"], "object")
            for case in ("short_research", "long_research", "short_research"):
                self.assertTrue(service.wait_quiescent())
                service.expected_case = case
                body = model_request(case)
                # An earlier completed flow must not advance the new flow's script.
                body["input"][:0] = [{"role": "user", "content": "Synthetic old request"}, {"type": "function_call_output", "call_id": "old", "output": "old"}]
                for name, call_id in (("web_search", "synthetic-search"), ("web_fetch", "synthetic-fetch")):
                    status, data = request(service.generation_endpoint, body)
                    self.assertEqual(status, 200)
                    item = events(data)[-1]["response"]["output"][0]
                    self.assertEqual((item["name"], item["call_id"], item["status"]), (name, call_id, "completed"))
                    args = json.loads(item["arguments"])
                    # Exa transport splits a multi-URL fetch into single-page MCP calls.
                    calls = [{"urls": [url]} for url in args["urls"]] if name == "web_fetch" else [args]
                    results = [rpc("tools/call", {"name": name + "_exa", "arguments": arguments}) for arguments in calls]
                    tool_result = results[0]
                    self.assertFalse(tool_result["isError"])
                    if name == "web_fetch":
                        self.assertEqual(len(results), len(args["urls"]))
                        for result, url in zip(results, args["urls"]):
                            block = result["content"][0]
                            self.assertIn("\nURL: " + url + "\n", block["text"])
                    body["input"].extend([item, tool_output(call_id, "search_excerpt" if name == "web_search" else "page_read", URLS if name == "web_search" else args["urls"])])
                status, data = request(service.generation_endpoint, body)
                self.assertEqual(status, 200)
                wire = events(data)
                text = "".join(event["delta"] for event in wire if event["type"] == "response.output_text.delta")
                if case == "long_research":
                    self.assertEqual(text, LONG_ANSWER)
                    self.assertGreaterEqual(len(text.split()), 200)
                    self.assertGreater(len(wire), 4)
                stats = service.snapshot()
                self.assertTrue(stats["terminal"])
                self.assertEqual((stats["model_rounds"], stats["searches"], stats["fetches"]), (3, 1, 1 if case == "short_research" else 2))
                self.assertEqual(stats["fetches"], stats["expected_fetches"])
                self.assertEqual(stats["readable_sources"], 1 if case == "short_research" else 2)
                self.assertEqual(request(service.generation_endpoint, body)[0], 400)
                self.assertFalse(service.snapshot()["terminal"])


if __name__ == "__main__":
    unittest.main()
