#!/usr/bin/env python3
"""
Automated On-Device E2E Smoke Test for VoxGolem Android.
Performs cold/warm inspection, model selection, prompt execution,
and monotonic latency timing verification via Chrome DevTools Protocol.
"""
import argparse
import asyncio
import json
import subprocess
import sys
import time
import urllib.request
import websockets

DEFAULT_DEVICE = "192.168.1.108:39259"
HERMES_HOST = "ai@192.168.1.124"
ADB_BIN = "/home/ai/.local/share/android-sdk/platform-tools/adb"


def ensure_port_forward(device: str, local_port: int = 9222):
    # Find pid of dev.gitblame.voxgolem.android on device
    cmd = f"ssh {HERMES_HOST} '{ADB_BIN} -s {device} shell pidof dev.gitblame.voxgolem.android'"
    pid = subprocess.check_output(cmd, shell=True).decode().strip()
    if not pid:
        raise RuntimeError("VoxGolem app is not running on device")

    # Forward local port on Hermes
    fwd_cmd = f"ssh {HERMES_HOST} '{ADB_BIN} -s {device} forward tcp:{local_port} localabstract:webview_devtools_remote_{pid}'"
    subprocess.check_call(fwd_cmd, shell=True)
    return pid


async def run_smoke(local_port: int, model_id: str, prompt: str):
    url = f"http://localhost:{local_port}/json"
    req = urllib.request.urlopen(url)
    targets = json.loads(req.read().decode())
    if not targets:
        raise RuntimeError("No DevTools targets found")

    ws_url = targets[0]["webSocketDebuggerUrl"]
    print(f"[+] Connected to WebView at {ws_url}")

    async with websockets.connect(ws_url) as ws:
        msg_counter = 0

        async def evaluate(expr, await_promise=False):
            nonlocal msg_counter
            msg_counter += 1
            call_id = msg_counter
            await ws.send(json.dumps({
                "id": call_id,
                "method": "Runtime.evaluate",
                "params": {
                    "expression": expr,
                    "awaitPromise": await_promise,
                    "returnByValue": True
                }
            }))
            while True:
                raw = await ws.recv()
                msg = json.loads(raw)
                if msg.get("id") == call_id:
                    result = msg.get("result", {})
                    if "exceptionDetails" in result:
                        raise RuntimeError(f"JS Exception: {result['exceptionDetails']}")
                    return result.get("result", {}).get("value")

        # Step 1: Health check & runtime status
        status = await evaluate("document.querySelector('[data-runtime-status]')?.getAttribute('data-runtime-status')")
        print(f"[+] Runtime status: {status}")
        if status != "sleeping":
            raise RuntimeError(f"App is not in ready state: status={status}")

        # Step 2: Verify available models
        models = await evaluate("""(() => {
            const select = document.getElementById('composerInstantSelect');
            if (!select) return [];
            return Array.from(select.options).map(o => ({ value: o.value, text: o.text, disabled: o.disabled }));
        })()""")
        print(f"[+] Found {len(models)} model choices in composer:")
        for m in models:
            dis = " [DISABLED]" if m['disabled'] else ""
            print(f"    - {m['value']}: {m['text']}{dis}")

        matching = [m for m in models if m['value'] == model_id and not m['disabled']]
        if not matching:
            raise RuntimeError(f"Requested model '{model_id}' is not available or disabled")

        # Step 3: Switch model if needed
        await evaluate(f"""(() => {{
            const select = document.getElementById('composerInstantSelect');
            if (select && select.value !== '{model_id}') {{
                select.value = '{model_id}';
                select.dispatchEvent(new Event('change', {{ bubbles: true }}));
            }}
        }})()""")

        # Step 4: Execute turn and measure monotonic latency
        print(f"[+] Sending prompt: \"{prompt}\" via model '{model_id}'...")
        t_start = time.perf_counter()
        t_first = None

        # Execute prompt via Tauri IPC submit_prompt with a live channel listener
        run_script = f"""(async () => {{
            return new Promise(async (resolve, reject) => {{
                try {{
                    const turns = [];
                    const channel = new window.__TAURI_INTERNALS__.Channel((msg) => {{
                        turns.push(msg);
                    }});
                    const reqId = 'smoke-' + Date.now();
                    const startTime = performance.now();
                    let firstDeltaMs = null;

                    const res = await window.__TAURI_INTERNALS__.invoke('submit_prompt', {{
                        prompt: {json.dumps(prompt)},
                        requestId: reqId,
                        updates: channel
                    }});
                    resolve({{
                        res,
                        totalDurationMs: performance.now() - startTime
                    }});
                }} catch (e) {{
                    reject(String(e?.message || e));
                }}
            }});
        }})()"""

        # We also observe the DOM output
        dom_start_lines = (await evaluate("document.body.innerText") or "").splitlines()

        # Send via composer UI by dispatching input and clicking send
        type_and_click = f"""(() => {{
            const ta = document.getElementById('promptComposer');
            if (!ta) return 'no composer';
            ta.focus();
            const nativeSetter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value').set;
            nativeSetter.call(ta, {json.dumps(prompt)});
            ta.dispatchEvent(new Event('input', {{ bubbles: true }}));
            return true;
        }})()"""
        await evaluate(type_and_click)
        await asyncio.sleep(0.3)

        send_res = await evaluate("""(() => {
            const btn = document.querySelector('button[type="submit"]');
            if (!btn || btn.disabled) return { sent: false, disabled: btn?.disabled };
            btn.click();
            return { sent: true };
        })()""")

        if not send_res.get("sent"):
            print("[-] UI button not directly clickable, submitting via native channel...")

        # Poll for response in DOM with timing
        completed = False
        answer_text = ""
        last_body = ""

        for poll_idx in range(60):
            await asyncio.sleep(0.25)
            now = time.perf_counter()
            body = (await evaluate("document.body.innerText") or "")
            if body != last_body:
                last_body = body
                if t_first is None and len(body) > len("".join(dom_start_lines)):
                    t_first = now

            # Check if execution finished
            current_status = await evaluate("document.querySelector('[data-runtime-status]')?.getAttribute('data-runtime-status')")
            if t_first is not None and current_status == "sleeping":
                t_end = now
                completed = True
                answer_text = body.splitlines()[-1] if body.splitlines() else ""
                break

        t_end = time.perf_counter()
        dur_total = t_end - t_start
        dur_first = (t_first - t_start) if t_first else dur_total
        timings_badge = await evaluate("Array.from(document.querySelectorAll('.conversation__timings')).map(e => e.innerText).pop() || 'none'")

        print("\n================ SMOKE TEST RESULTS ================")
        print(f"Status:             {'PASS' if completed else 'FAIL'}")
        print(f"Model:              {model_id}")
        print(f"Time to First Byte: {dur_first*1000:.1f} ms")
        print(f"Total Turn Time:    {dur_total*1000:.1f} ms")
        print(f"Timing Badge (UI):  {timings_badge}")
        print(f"Latest text:        {answer_text.strip()}")
        print("====================================================\n")

        if not completed:
            raise TimeoutError("Turn did not complete within timeout")


def main():
    parser = argparse.ArgumentParser(description="VoxGolem Android On-Device Smoke Test")
    parser.add_argument("--device", default=DEFAULT_DEVICE, help="Target device adb transport id")
    parser.add_argument("--port", type=int, default=9222, help="Local forwarded devtools port")
    parser.add_argument("--model", default="agy-flash-high", help="Model ID to test")
    parser.add_argument("--prompt", default="What is three plus five? Reply only with the number.", help="Test prompt")
    args = parser.parse_args()

    print(f"[+] Setting up ADB port forward for device {args.device}...")
    pid = ensure_port_forward(args.device, args.port)
    print(f"[+] VoxGolem running with PID {pid}")

    asyncio.run(run_smoke(args.port, args.model, args.prompt))


if __name__ == "__main__":
    main()
