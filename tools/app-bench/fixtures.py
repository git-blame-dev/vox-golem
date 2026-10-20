"""Synthetic audio and bounded loopback Responses/MCP fixtures (stdlib only)."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import struct
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

RATE = 16000
MAX_BODY = 256 * 1024
MODEL = "gpt-6-luna"
STREAM_DELAY_MS = 400
STREAM_GATE_TIMEOUT_SECONDS = 8
CASES = {
    "quick": "What is seventeen plus twenty five?",
    "short_research": "What Rust release introduced Once Lock? Read the official documentation and give a brief sourced answer.",
    "long_research": "Research Once Lock and Lazy Lock in Rust. Read the official documentation and compare their release versions, initialization timing, panic behavior, and ability to reset. Explain the important differences in several short paragraphs with sources.",
}
URLS = ["https://benchmark.example.test/once-lock", "https://benchmark.example.test/lazy-lock"]
PAGES = [
    "Synthetic documentation fixture: OnceLock became stable in Rust 1.70. Initialization happens on an explicit get_or_init call. A panicking initializer leaves the cell uninitialized. take requires mutable access and resets the cell.",
    "Synthetic documentation fixture: LazyLock became stable in Rust 1.80. It stores its initializer and initializes on first dereference. Initialization panic poisons the lazy value. It does not provide the same stable reset operation as OnceLock.",
]
LONG_ANSWER = (
    "In these synthetic documentation fixtures, OnceLock became stable in Rust 1.70, whereas LazyLock became stable in Rust 1.80. Both represent values initialized once and shared safely between threads. The different release versions matter when selecting a minimum supported Rust version. A project supporting older compilers cannot assume that both types are available merely because one of them is available.\n\n"
    "OnceLock makes the initialization decision explicit. Its get_or_init method accepts the initialization function at the call site. Reading the cell with get does not itself initialize the value. LazyLock instead stores its initialization function when the lazy wrapper is constructed. Access through dereferencing triggers initialization when the value is first needed. This distinction changes where initialization dependencies and work are expressed, even when the resulting stored value is identical.\n\n"
    "Panic behavior also differs. A panic during OnceLock initialization leaves the cell uninitialized, so a later attempt can try again. LazyLock initialization panic poisons the lazy value; future access does not simply rerun a replacement initializer. Neither type should be described as providing general recovery from every failure in application initialization.\n\n"
    "Finally, OnceLock can be reset using take when the caller has exclusive mutable access. That requirement preserves the guarantees made to shared readers. LazyLock does not offer the same stable reset operation in these fixtures. Choose explicit initialization and reset ownership when they are required, rather than treating these wrappers as interchangeable caches. These are synthetic benchmark sources, not live documentation retrieved from the Internet."
)


def matches_transcript(case, text):
    """Match critical intent, allowing punctuation, identifier spelling and numbers."""
    if case not in CASES or not isinstance(text, str):
        return False
    value = re.sub(r"[^a-z0-9]+", " ", text.lower()).strip()
    value = re.sub(r"\b(?:once|one(?:\s+s)?)\s*l(?:ock(?:ed)?|uck)\b", "oncelock", value)
    value = re.sub(r"\blazy\s*lock(?:ed)?\b", "lazylock", value)
    value = re.sub(r"\bseventeen\b", "17", value)
    value = re.sub(r"\btwenty five\b", "25", value)
    tokens = set(value.split())
    groups = {
        "quick": [{"17"}, {"25"}, {"plus", "add", "sum"}],
        "short_research": [{"rust"}, {"release", "version"}, {"introduced", "introduce"}, {"oncelock"}, {"official"}, {"documentation", "docs"}, {"brief", "short"}, {"sourced", "sources", "source"}, {"answer"}],
        "long_research": [{"research"}, {"oncelock"}, {"lazylock"}, {"rust"}, {"official"}, {"documentation", "docs"}, {"compare"}, {"release", "versions", "version"}, {"initialization", "initialisation"}, {"timing"}, {"panic"}, {"behavior", "behaviour"}, {"reset"}, {"differences"}, {"paragraphs"}, {"sources", "source"}],
    }
    return all(tokens & alternatives for alternatives in groups[case])


def expected_answer(case):
    if case == 'quick':
        return '42'
    if case == 'short_research':
        return 'OnceLock became stable in Rust 1.70, according to the synthetic documentation fixture.'
    if case == 'long_research':
        return LONG_ANSWER
    raise ValueError('Unknown synthetic case')


def generate_fixtures(directory, voice="en-gb", speed=160, wake_phrase="hey livekit"):
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    source = Path(__file__).resolve().parents[1] / "wakeword-bench" / "bench.py"
    spec = importlib.util.spec_from_file_location("wakeword_fixture_generator", source)
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)

    def spoken(text, temporary):
        try:
            samples = helper.pcm_from_espeak(text, voice, speed, temporary)
        except Exception:
            # The reused helper's error includes command arguments and stderr.
            raise ValueError("synthetic_audio_generation_failed") from None
        while samples and samples[-1] == 0:
            samples.pop()
        if not samples:
            raise ValueError("empty_synthetic_audio")
        return samples

    result = {}
    with tempfile.TemporaryDirectory(prefix="speech-", dir=directory) as temporary:
        temporary = Path(temporary)
        wake = spoken(wake_phrase, temporary)
        for case, question in CASES.items():
            speech = spoken(question, temporary)
            wake_end = RATE * 2 + len(wake)
            start = wake_end + int(RATE * 0.35)
            samples = [0] * (RATE * 2) + wake + [0] * int(RATE * 0.35) + speech
            data = struct.pack("<" + "f" * len(samples), *(sample / 32768 for sample in samples))
            (directory / f"{case}.pcm").write_bytes(data)
            result[case] = dict(byte_count=len(data), sample_count=len(samples),
                                duration_seconds=len(samples) / RATE, wake_end_sample=wake_end,
                                question_start_sample=start, question_end_sample=len(samples),
                                sha256=hashlib.sha256(data).hexdigest())
    return result


class _Server(ThreadingHTTPServer):
    daemon_threads = False
    block_on_close = True

    def handle_error(self, request, client_address):
        self.owner._increment("failures")


class FakeServices:
    """One loopback server; select expected_case before each independent flow.

    snapshot is per-flow and contains only numeric counters and boolean flags.
    Endpoints are configuration only and deliberately excluded from snapshots.
    """
    def __init__(self, gated_stream=False):
        self._lock = threading.RLock()
        self._quiescent = threading.Condition(self._lock)
        self._active_generations = 0
        self._gated_stream = bool(gated_stream)
        self._server = None
        self._thread = None
        self.expected_case = "quick"

    @property
    def expected_case(self):
        return self._case

    @expected_case.setter
    def expected_case(self, case):
        if case not in CASES:
            raise ValueError("unknown_case")
        with self._lock:
            if self._active_generations:
                raise RuntimeError("generation_active")
            self._case = case
            self._stream_gate = threading.Event()
            self._expected_history = None
            self._stats = dict(generation_requests=0, model_rounds=0, searches=0, fetches=0,
                               readable_sources=0, initializations=0, discoveries=0, failures=0,
                               transcript_matches=False, model_matches=False, terminal=False,
                               expected_model_rounds=1 if case == "quick" else 3,
                               expected_searches=0 if case == "quick" else 1,
                               expected_fetches=0 if case == "quick" else (1 if case == "short_research" else 2),
                                stream_delay_ms=STREAM_DELAY_MS,
                                stream_gate_enabled=self._gated_stream,
                                stream_gate_wait_ms=0, stream_gate_timed_out=False,
                                first_delta_sent=False, first_delta_sent_monotonic=0,
                                history_matches=True, history_turns_expected=0, history_turns_observed=0,
                                expected_readable_sources=0 if case == "quick" else (1 if case == "short_research" else 2))

    def release_stream(self):
        with self._lock:
            self._stream_gate.set()

    def wait_quiescent(self, timeout=3):
        with self._quiescent:
            return self._quiescent.wait_for(lambda: self._active_generations == 0, timeout)

    def expect_history(self, turns=None):
        """Compare canonical snapshot text without modifying the request history."""
        expected = None
        if turns is not None:
            try:
                expected = []
                for turn in turns:
                    for role, key in (("user", "user"), ("assistant", "answer")):
                        content = turn[key]["content"]
                        text = content["text"]
                        if not isinstance(text, str):
                            raise ValueError()
                        if key == "answer" and content.get("kind") != "refusal" and "\n\nSources:\n" not in text:
                            sources = turn.get("sources")
                            if isinstance(sources, list):
                                urls = [
                                    item["url"].strip()
                                    for item in sources
                                    if isinstance(item, dict)
                                    and isinstance(item.get("url"), str)
                                    and item["url"].strip()
                                ]
                                if urls:
                                    text = f"{text}\n\nSources:\n" + "\n".join(f"- {url}" for url in urls)
                        expected.append((role, text))
            except (KeyError, TypeError, ValueError):
                raise ValueError("invalid_expected_history") from None
        with self._lock:
            if self._active_generations:
                raise RuntimeError("generation_active")
            self._expected_history = expected
            self._stats.update(history_matches=True, history_turns_expected=0 if expected is None else len(expected) // 2,
                               history_turns_observed=0)

    def _increment(self, key, count=1):
        with self._lock:
            self._stats[key] += count
            if key == "failures":
                self._stats["terminal"] = False

    def snapshot(self):
        with self._lock:
            return dict(self._stats)

    def __enter__(self):
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(2)

            def log_message(self, *args):
                pass

            def send(self, status, value=None, content_type="application/json"):
                data = b"" if value is None else (value if isinstance(value, bytes) else json.dumps(value).encode())
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                # A documented fixture schedule, not simulated application progress:
                # flush one real text event, then send the finite remainder 400 ms later.
                paced = content_type == "text/event-stream" and data.startswith(b'data: {"type": "response.output_text.delta"')
                if paced:
                    boundary = data.index(b"\r\n\r\n") + 4
                    self.wfile.write(data[:boundary])
                    self.wfile.flush()
                    with owner._lock:
                        started = time.monotonic()
                        owner._stats.update(first_delta_sent=True, first_delta_sent_monotonic=started)
                    if owner._gated_stream:
                        released = owner._stream_gate.wait(STREAM_GATE_TIMEOUT_SECONDS)
                        with owner._lock:
                            owner._stats.update(stream_gate_wait_ms=(time.monotonic() - started) * 1000,
                                                stream_gate_timed_out=not released)
                            if not released:
                                owner._increment("failures")
                    else:
                        time.sleep(STREAM_DELAY_MS / 1000)
                    self.wfile.write(data[boundary:])
                    self.wfile.flush()
                    with owner._lock:
                        owner._stats["terminal"] = owner._stats["failures"] == 0
                else:
                    self.wfile.write(data)
                self.close_connection = True

            def do_GET(self):
                self.send(405)

            def do_HEAD(self):
                self.send(200)

            def do_DELETE(self):
                self.send(200)

            def do_POST(self):
                generation = self.path == "/responses"
                if generation:
                    with owner._quiescent:
                        owner._active_generations += 1
                try:
                    if self.headers.get("Transfer-Encoding"):
                        raise ValueError()
                    length = int(self.headers.get("Content-Length", "-1"))
                    if not 0 <= length <= MAX_BODY:
                        owner._increment("failures")
                        self.send(413, {"error": {"type": "fixture_body_limit"}})
                        return
                    raw = self.rfile.read(length)
                    if len(raw) != length:
                        raise ValueError()
                    body = json.loads(raw)
                    if not isinstance(body, dict):
                        raise ValueError()
                    with owner._lock:
                        if self.path == "/responses":
                            status, response = owner._generation(body)
                            content_type = "text/event-stream" if status == 200 else "application/json"
                        elif self.path == "/mcp":
                            status, response = owner._mcp(body)
                            content_type = "application/json"
                        else:
                            status, response, content_type = 404, None, "application/json"
                    self.send(status, response, content_type)
                except (ValueError, TypeError, KeyError, RecursionError):
                    owner._increment("failures")
                    self.send(400, {"error": {"type": "fixture_invalid_request"}})
                except (OSError, TimeoutError):
                    owner._increment("failures")
                finally:
                    if generation:
                        with owner._quiescent:
                            owner._active_generations -= 1
                            owner._quiescent.notify_all()

        self._server = _Server(("127.0.0.1", 0), Handler)
        self._server.owner = self
        base = f"http://127.0.0.1:{self._server.server_port}"
        self.generation_endpoint = base + "/responses"
        self.mcp_endpoint = base + "/mcp"
        self._thread = threading.Thread(target=self._server.serve_forever, name="synthetic-fixtures")
        self._thread.start()
        return self

    def __exit__(self, *args):
        self.release_stream()
        self._server.shutdown()
        self._server.server_close()
        self._thread.join()

    def _generation(self, body):
        self._increment("generation_requests")
        inputs = body.get("input", [])
        if not isinstance(inputs, list) or not all(isinstance(item, dict) for item in inputs):
            raise ValueError()
        users = [item for item in inputs if item.get("role") == "user"]
        content = users[-1].get("content", []) if users else []
        if not isinstance(content, (str, list)) or not isinstance(body.get("reasoning"), dict):
            raise ValueError()
        text = content if isinstance(content, str) else " ".join(
            part.get("text", "") for part in content if isinstance(part, dict) and part.get("type") == "input_text")
        valid_prompt = matches_transcript(self._case, text)
        valid_model = body.get("model") == MODEL and body.get("reasoning", {}).get("effort") == "none"
        self._stats.update(transcript_matches=valid_prompt, model_matches=valid_model)
        if not valid_prompt or not valid_model:
            self._increment("failures")
            self._stats["terminal"] = False
            return 400, {"error": {"type": "fixture_contract_mismatch"}}
        # Count only the current user's continuation, never earlier warm-flow history.
        last_user = max(i for i, item in enumerate(inputs) if item.get("role") == "user")
        if self._expected_history is not None:
            self._stats["history_matches"] = False
            observed = []
            for item in inputs[:last_user]:
                role = item.get("role")
                if role not in {"user", "assistant"}:
                    continue
                content = item.get("content")
                if isinstance(content, list):
                    if not all(isinstance(part, dict) and part.get("type") in {"input_text", "output_text"}
                               and isinstance(part.get("text"), str) for part in content):
                        raise ValueError()
                    content = "".join(part["text"] for part in content)
                if not isinstance(content, str):
                    raise ValueError()
                observed.append((role, content))
            matches = observed == self._expected_history
            self._stats.update(history_matches=matches,
                               history_turns_observed=sum(role == "user" for role, _ in observed))
            if not matches:
                self._increment("failures")
                return 400, {"error": {"type": "fixture_contract_mismatch"}}
        outputs = [item for item in inputs[last_user + 1:] if item.get("type") == "function_call_output"]
        expected_ids = ["synthetic-search", "synthetic-fetch"][:len(outputs)]
        if len(outputs) > 2 or [item.get("call_id") for item in outputs] != expected_ids:
            raise ValueError()
        round_number = len(outputs)
        if (self._stats["failures"] or self._stats["terminal"]
                or self._stats["model_rounds"] >= self._stats["expected_model_rounds"]
                or round_number != self._stats["model_rounds"]
                or (self._case == "quick" and outputs)):
            raise ValueError()
        for index, output in enumerate(outputs):
            self._validate_evidence(output, "search_excerpt" if index == 0 else "page_read")
        self._increment("model_rounds")
        events = []
        if self._case != "quick" and round_number < 2:
            search = round_number == 0
            name = "web_search" if search else "web_fetch"
            args = {"objective": "Synthetic Rust documentation comparison", "queries": ["Rust OnceLock LazyLock documentation"], "num_results": 2} if search else {"urls": URLS[:1 if self._case == "short_research" else 2], "focus": "Release, initialization, panic and reset semantics"}
            call_id = "synthetic-search" if search else "synthetic-fetch"
            items = [{"type": "function_call", "id": "synthetic-item-" + call_id,
                      "call_id": call_id, "name": name, "arguments": json.dumps(args), "status": "completed"}]
        else:
            text = expected_answer(self._case)
            chunk_size = 1 if self._case == "quick" else 96
            for start in range(0, len(text), chunk_size):
                events.append({"type": "response.output_text.delta", "delta": text[start:start + chunk_size]})
            items = [{"type": "message", "id": "synthetic-answer", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": text}]}]
        events.extend({"type": "response.output_item.done", "item": item} for item in items)
        events.append({"type": "response.completed", "response": {"id": "synthetic-response", "model": MODEL, "status": "completed", "output": items, "usage": {"input_tokens": 100, "output_tokens": 80, "total_tokens": 180}}})
        return 200, "".join("data: " + json.dumps(event) + "\r\n\r\n" for event in events).encode()

    def _validate_evidence(self, output, kind):
        # Production search.rs wraps serialized core::search::ToolResult here.
        payload = json.loads(output.get("output", ""))
        if not isinstance(payload, dict) or "error" in payload:
            raise ValueError()
        evidence = payload.get("untrusted_web_evidence")
        if (not isinstance(evidence, dict) or evidence.get("partial") is not False
                or evidence.get("evidence_unavailable") is not False
                or evidence.get("failures") != []):
            raise ValueError()
        sources = evidence.get("sources")
        if not isinstance(sources, list) or not sources:
            raise ValueError()
        urls = []
        for source in sources:
            if (not isinstance(source, dict) or source.get("kind") != kind
                    or source.get("truncated") is not False
                    or not isinstance(source.get("text"), str)
                    or not any(char.isprintable() and not char.isspace() for char in source["text"])):
                raise ValueError()
            urls.append(source.get("url"))
        expected = URLS[:1 if self._case == "short_research" else 2]
        if kind == "page_read":
            if len(urls) != len(expected) or set(urls) != set(expected):
                raise ValueError()
        elif any(url not in URLS for url in urls):
            raise ValueError()

    def _mcp(self, body):
        method = body.get("method")
        result = {}
        if method == "initialize":
            version = body.get("params", {}).get("protocolVersion")
            if version not in {"2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"}:
                raise ValueError()
            # This fixture implements the initialized, stateless 2025 protocol;
            # a newer client offer negotiates that version, not an invented
            # implementation of the newer no-initialize transport.
            version = "2025-11-25" if version == "2026-07-28" else version
            self._increment("initializations")
            result = {"protocolVersion": version, "capabilities": {"tools": {}}, "serverInfo": {"name": "synthetic-benchmark", "version": "1"}}
        elif method == "notifications/initialized":
            return 202, None
        elif method == "tools/list":
            self._increment("discoveries")
            result = {"tools": []}
            for name in ("web_search_exa", "web_fetch_exa", "web_search", "web_fetch"):
                search = "search" in name
                properties = {"objective": {"type": "string"}, "query": {"type": "string"}, "search_queries": {"type": "array", "items": {"type": "string"}}, "numResults": {"type": "integer"}} if search else {"urls": {"type": "array", "items": {"type": "string"}}, "objective": {"type": "string"}, "maxCharacters": {"type": "integer"}}
                result["tools"].append({"name": name, "inputSchema": {"type": "object", "properties": properties, "required": ["query" if name.endswith("exa") and search else "objective" if search else "urls"]}})
        elif method == "tools/call":
            params = body.get("params", {})
            name = params.get("name")
            if name not in {"web_search_exa", "web_search", "web_fetch_exa", "web_fetch"}:
                raise ValueError()
            search = "search" in name
            selected = URLS if search else params.get("arguments", {}).get("urls", [])
            if not selected or not isinstance(selected, list) or any(url not in URLS for url in selected):
                raise ValueError()
            self._increment("searches" if search else "fetches")
            if not search:
                self._increment("readable_sources", len(selected))
            pages = [{"url": url, "title": "Synthetic Rust documentation", "excerpts": [PAGES[URLS.index(url)]], "text": PAGES[URLS.index(url)]} for url in selected]
            # Exa fetch decoding treats each text block as one provider-owned page.
            result = {"isError": False, "structuredContent": {"results": pages, "errors": []}, "content": [{"type": "text", "text": "Title: Synthetic Rust documentation\nURL: " + page["url"] + "\nPublished: N/A\nAuthor: N/A\nHighlights:\n" + page["text"]} for page in pages]}
        else:
            raise ValueError()
        return 200, {"jsonrpc": "2.0", "id": body.get("id"), "result": result}
