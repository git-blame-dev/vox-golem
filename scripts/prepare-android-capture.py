#!/usr/bin/env python3
from pathlib import Path


root = Path(__file__).resolve().parent.parent
build_file = root / "apps/android-tauri/src-tauri/gen/android/app/build.gradle.kts"
hook = 'apply(from = "../../../../capture.gradle.kts")'
content = build_file.read_text()
if hook not in content:
    generator_hook = 'apply(from = "tauri.build.gradle.kts")'
    if content.count(generator_hook) != 1:
        raise SystemExit("Cannot locate the pinned Android app build hook")
    build_file.write_text(content.replace(generator_hook, hook + "\n" + generator_hook))
