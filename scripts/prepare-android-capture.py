#!/usr/bin/env python3
from pathlib import Path
import re


def with_hook(content: str, hook: str, anchor: str, label: str) -> str:
    lines = content.splitlines()
    anchors = [index for index, line in enumerate(lines) if line.strip() == anchor]
    anchor_name = 'tauri.build.gradle.kts' if label == 'app build' else 'tauri.settings.gradle'
    if len(anchors) != 1 or content.count(anchor_name) != 1:
        raise ValueError(f"Cannot locate the single pinned Android {label} hook")
    hook_name = 'capture.gradle.kts' if label == 'app build' else 'capture.settings.gradle'
    hooks = [index for index, line in enumerate(lines) if hook_name in line]
    if hooks:
        if len(hooks) != 1 or lines[hooks[0]].strip() != hook or hooks[0] >= anchors[0]:
            raise ValueError(f"Cannot prepare Android {label}: duplicate, conflicting or misplaced capture hook")
        return content
    newline = '\r\n' if '\r\n' in content else '\n'
    return content.replace(anchor, hook + newline + anchor, 1)


def prepare(root: Path) -> None:
    android = root / 'apps/android-tauri/src-tauri/gen/android'
    build_file = android / 'app/build.gradle.kts'
    settings_file = android / 'settings.gradle'
    properties_file = android / 'gradle.properties'
    try:
        build = build_file.read_bytes().decode('utf-8')
        settings = settings_file.read_bytes().decode('utf-8')
        properties = properties_file.read_bytes().decode('utf-8') if properties_file.exists() else ''
    except (OSError, UnicodeError) as error:
        raise ValueError(f"Cannot read the pinned Android scaffold: {error}") from error
    if ':capture-android' in settings:
        raise ValueError('Cannot prepare Android settings: capture-android mapping must remain in the authored hook')
    prepared_build = with_hook(build, 'apply(from = "../../../../capture.gradle.kts")',
                               'apply(from = "tauri.build.gradle.kts")', 'app build')
    prepared_settings = with_hook(settings, "apply from: '../../../capture.settings.gradle'",
                                   "apply from: 'tauri.settings.gradle'", 'settings')
    prepared_properties = properties
    if re.search(r'(?m)^\s*org\.gradle\.caching(?:\s*[=:]|\s+)', properties) is None:
        newline = '\r\n' if '\r\n' in properties else '\n'
        prepared_properties += ('' if not properties or properties.endswith('\n') else newline) + 'org.gradle.caching=true' + newline
    for path, original, prepared in ((build_file, build, prepared_build),
                                     (settings_file, settings, prepared_settings),
                                     (properties_file, properties, prepared_properties)):
        if prepared != original:
            path.write_bytes(prepared.encode('utf-8'))


if __name__ == '__main__':
    try:
        prepare(Path(__file__).resolve().parent.parent)
    except ValueError as error:
        raise SystemExit(str(error))
