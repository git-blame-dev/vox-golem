#!/usr/bin/env python3
import argparse
import copy
import json
import math
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parent.parent
DESKTOP_ICONS = ROOT / "apps/desktop-tauri/src-tauri/icons"
ANDROID_SHELL = ROOT / "apps/android-tauri/src-tauri"
SVG = "{http://www.w3.org/2000/svg}"
ANDROID_ICON_SIZES = {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}
ET.register_namespace("", SVG[1:-1])


def android_layer(source):
    layer = copy.deepcopy(source)
    x, y, width, height = map(float, source.attrib["viewBox"].split())
    padded_width = width * 108 / 72
    padded_height = height * 108 / 72
    layer.set("width", "108")
    layer.set("height", "108")
    layer.set(
        "viewBox",
        f"{x - (padded_width - width) / 2:g} "
        f"{y - (padded_height - height) / 2:g} "
        f"{padded_width:g} {padded_height:g}",
    )
    return layer


def monochrome_layer(source):
    layer = android_layer(source)
    layer.set("color", "#FFFFFF")
    mark = layer.find(f"{SVG}g[@id='mark']")
    if mark is None:
        raise ValueError("The icon must contain its artwork in the mark group")
    for shape in list(mark):
        if shape.tag == SVG + "defs":
            continue
        if shape.get("fill") == "#000000" and shape.get("stroke") == "none":
            mark.remove(shape)
        elif shape.get("fill") not in (None, "none"):
            shape.set("fill", "#FFFFFF")
    return layer


def entry_layer(source, purpose):
    color = source.get("color", "")
    if re.fullmatch(r"#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})", color) is None:
        raise ValueError("The shortcut icon requires an opaque hex root color (#RGB or #RRGGBB)")
    layer = copy.deepcopy(source)
    mark = layer.find(f"{SVG}g[@id='mark']")
    if mark is None:
        raise ValueError("The icon must contain its artwork in the mark group")
    fraction = artwork_geometry(source)
    x, y, width, height = map(float, source.attrib["viewBox"].split())
    cx, cy = x + width / 2, y + height / 2
    radius = width * fraction / 2
    mark_index = list(layer).index(mark)
    layer.remove(mark)
    scaled = ET.Element(SVG + "g", {
        "id": "shortcut-mark",
        "transform": f"translate({cx:g} {cy:g}) scale(0.82) translate({-cx:g} {-cy:g})",
    })
    scaled.append(mark)
    layer.insert(mark_index, scaled)
    badge = ET.SubElement(layer, SVG + "g", {
        "id": "entry-badge",
        "transform": f"translate({cx + radius * .5:g} {cy + radius * .5:g}) scale({radius / 28:g})",
    })
    ET.SubElement(badge, SVG + "circle", {
        "cx": "0", "cy": "0", "r": "7", "fill": "#000000", "stroke": "none",
    })
    glyphs = {
        "ask": "M-2-1V-3A2 2 0 0 1 2-3V-1A2 2 0 0 1-2-1Z M-4-1V0A4 4 0 0 0 4 0V-1 M0 4V6 M-2 6H2",
        "chat": "M-4-3H4V2H0L-3 5V2H-4Z",
        "talk": "M-5-4H2V0H-1L-3 2V0H-5Z M4-1H5V4H3L1 6V4H-1V3",
        "note": "M-3-5H1L4-2V5H-3Z M1-5V-2H4 M-1 0H2 M-1 2H2",
    }
    if purpose not in glyphs:
        raise ValueError(f"Unknown entry purpose: {purpose}")
    ET.SubElement(badge, SVG + "path", {
        "d": glyphs[purpose], "fill": "none", "stroke": color,
        "stroke-width": "1.4", "stroke-linecap": "round", "stroke-linejoin": "round",
    })
    return layer


def phone_shortcut_layer(source):
    return entry_layer(source, "ask")


def render_entries(source, work, destination, purposes):
    # Render each purpose once; the legacy pinned shortcut aliases Ask's bytes.
    layers = {purpose: entry_layer(source, purpose) for purpose in purposes}
    outputs = {}
    for purpose, layer in layers.items():
        derived = work / f"entry-{purpose}.svg"
        ET.ElementTree(layer).write(derived, encoding="unicode")
        generated = work / f"entry-{purpose}"
        subprocess.run(
            ["cargo", "tauri", "icon", str(derived), "--output", str(generated)],
            cwd=ROOT, check=True,
        )
        outputs[purpose] = generated / "icon.png"
    directory = destination / "drawable-nodpi"
    directory.mkdir(parents=True, exist_ok=True)
    for purpose, png in outputs.items():
        shutil.copyfile(png, directory / f"voxgolem_entry_{purpose}.png")
    return outputs


def artwork_geometry(source):
    """Fit the canonical centered circular mark, rejecting geometry we cannot measure."""
    x, y, width, height = map(float, source.attrib["viewBox"].split())
    mark = source.find(f"{SVG}g[@id='mark']")
    if mark is None or mark.get("transform") or source.get("transform"):
        raise ValueError("Artwork geometry requires an untransformed mark group")
    if not all(math.isfinite(value) for value in (x, y, width, height)) or width <= 0 or width != height:
        raise ValueError("Artwork geometry requires a finite square canvas")
    radii = []
    for shape in mark:
        if shape.tag == SVG + "defs":
            continue
        if shape.tag != SVG + "circle" or shape.get("transform"):
            raise ValueError("Artwork geometry requires centered untransformed circles")
        cx, cy, radius = (float(shape.get(key, "0")) for key in ("cx", "cy", "r"))
        stroke = shape.get("stroke", mark.get("stroke", source.get("stroke", "none")))
        stroke_width = float(shape.get("stroke-width", mark.get("stroke-width", source.get("stroke-width", "1"))))
        if not all(math.isfinite(value) for value in (cx, cy, radius, stroke_width)) or radius <= 0 or stroke_width < 0:
            raise ValueError("Artwork geometry requires finite positive circles")
        if not math.isclose(cx, x + width / 2, abs_tol=1e-6) or not math.isclose(cy, y + height / 2, abs_tol=1e-6):
            raise ValueError("Artwork geometry requires circles centered on the canvas")
        fill = shape.get("fill", mark.get("fill", source.get("fill", "black")))
        if stroke != "none" or fill != "none":
            radii.append(radius + (stroke_width / 2 if stroke != "none" else 0))
    fraction = 2 * max(radii, default=0) / width
    if not 0 < fraction <= 1:
        raise ValueError("Artwork ink must fit inside its canvas")
    return fraction


def artwork_geometry_resource(source):
    fraction = artwork_geometry(source)
    return ("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<resources>\n"
            f"    <item type=\"fraction\" name=\"artwork_ink_fraction\">{fraction * 100:.9f}%</item>\n"
            "</resources>\n")


def main():
    parser = argparse.ArgumentParser(description="Generate icons from the canonical desktop SVG")
    output_modes = parser.add_mutually_exclusive_group()
    output_modes.add_argument(
        "--wear-res-output",
        type=Path,
        metavar="PATH",
        help="Generate Wear launcher, entry icons and capture background resources into PATH",
    )
    output_modes.add_argument(
        "--phone-shortcut-res-output",
        type=Path,
        metavar="PATH",
        help="Generate phone Chat/Ask/Talk entry icons and legacy Ask shortcut into PATH",
    )
    arguments = parser.parse_args()
    version = subprocess.check_output(
        ["cargo", "tauri", "--version"], text=True
    ).strip()
    if version != "tauri-cli 2.11.1":
        raise SystemExit("Icon generation requires tauri-cli 2.11.1")

    source_path = DESKTOP_ICONS / "icon.svg"
    source = ET.parse(source_path).getroot()
    geometry = None
    if arguments.wear_res_output is not None:
        try:
            geometry = artwork_geometry_resource(source)
        except ValueError as error:
            parser.error(str(error))
    target = None
    if arguments.wear_res_output is None and arguments.phone_shortcut_res_output is None:
        target = ROOT / "target"
        target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="icons-", dir=target) as temporary:
        work = Path(temporary)
        if arguments.phone_shortcut_res_output is not None:
            try:
                outputs = render_entries(source, work, arguments.phone_shortcut_res_output, ("chat", "ask", "talk"))
            except ValueError as error:
                parser.error(str(error))
            output = arguments.phone_shortcut_res_output / "drawable-nodpi/voxgolem_new_voice_shortcut.png"
            shutil.copyfile(outputs["ask"], output)
            print(f"Generated phone shortcut resource {output} from icons/icon.svg")
            return
        foreground = work / "foreground.svg"
        monochrome = work / "monochrome.svg"
        ET.ElementTree(android_layer(source)).write(foreground, encoding="unicode")
        ET.ElementTree(monochrome_layer(source)).write(monochrome, encoding="unicode")
        manifest = work / "icons.json"
        manifest.write_text(
            json.dumps(
                {
                    "default": str(source_path),
                    "bg_color": "#000000",
                    "android_fg": str(foreground),
                    "android_monochrome": str(monochrome),
                }
            )
            + "\n"
        )
        generated = work / "generated"
        subprocess.run(
            ["cargo", "tauri", "icon", str(manifest), "--output", str(generated)],
            cwd=ROOT,
            check=True,
        )
        legacy = work / "legacy"
        subprocess.run(
            [
                "cargo", "tauri", "icon", str(source_path),
                "--png", ",".join(map(str, ANDROID_ICON_SIZES.values())),
                "--output", str(legacy),
            ],
            cwd=ROOT,
            check=True,
        )
        for density, size in ANDROID_ICON_SIZES.items():
            directory = generated / "android" / f"mipmap-{density}"
            for filename in ("ic_launcher.png", "ic_launcher_round.png"):
                shutil.copyfile(legacy / f"{size}x{size}.png", directory / filename)
        if arguments.wear_res_output is not None:
            destination = arguments.wear_res_output
            try:
                render_entries(source, work, destination, ("chat", "ask", "talk", "note"))
            except ValueError as error:
                parser.error(str(error))
            android = generated / "android"
            launcher_files = list(android.glob("mipmap-*/ic_launcher*.png")) + [
                android / "mipmap-anydpi-v26/ic_launcher.xml",
                android / "values/ic_launcher_background.xml",
            ]
            for path in launcher_files:
                output = destination / path.relative_to(android)
                output.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, output)
            background = destination / "drawable-nodpi/capture_background.png"
            background.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(generated / "icon.png", background)
            geometry_output = destination / "values/artwork_geometry.xml"
            geometry_output.parent.mkdir(parents=True, exist_ok=True)
            geometry_output.write_text(geometry, encoding="utf-8")
            print(f"Generated Wear resources in {destination} from icons/icon.svg")
            return
        for filename in ("icon.png", "icon.ico"):
            shutil.copyfile(generated / filename, DESKTOP_ICONS / filename)

        android_icons = ANDROID_SHELL / "icons"
        android_icons.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(generated / "icon.png", android_icons / "icon.png")
        resources = ANDROID_SHELL / "gen/android/app/src/main/res"
        destination = resources if resources.is_dir() else android_icons / "android"
        for path in (generated / "android").rglob("*"):
            if path.is_file():
                output = destination / path.relative_to(generated / "android")
                output.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, output)
    print("Generated desktop and Android icons from icons/icon.svg")


if __name__ == "__main__":
    main()
