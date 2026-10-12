import contextlib
import hashlib
import importlib.util
import io
import json
import math
from pathlib import Path
import shutil
import subprocess
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("generate-icons.py")
SPEC = importlib.util.spec_from_file_location("generate_icons", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot import icon generator")
icons = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(icons)
CANONICAL_SVG = icons.DESKTOP_ICONS / "icon.svg"
SYNTHETIC_SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="10 20 60 60" color="#12ABCD">
<g id="mark"><circle cx="40" cy="50" fill="#000000" stroke="none" r="30"/>
<circle cx="40" cy="50" fill="#F74638" r="20"/></g></svg>"""
LAUNCHER_FILES = tuple(
    f"mipmap-{density}/{filename}"
    for density in icons.ANDROID_ICON_SIZES
    for filename in (
        "ic_launcher.png", "ic_launcher_round.png",
        "ic_launcher_foreground.png", "ic_launcher_monochrome.png",
    )
) + ("mipmap-anydpi-v26/ic_launcher.xml", "values/ic_launcher_background.xml")


def file_bytes(directory):
    return {str(path.relative_to(directory)): path.read_bytes()
            for path in directory.rglob("*") if path.is_file()}


class IconGenerationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="test-icons-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.desktop = self.root / "desktop/icons"
        self.phone = self.root / "phone"
        self.desktop.mkdir(parents=True)
        (self.desktop / "icon.svg").write_text(SYNTHETIC_SVG)
        self.calls = []

    def fake_render(self, command, **kwargs):
        self.calls.append(command)
        output = Path(command[command.index("--output") + 1])
        output.mkdir()
        if "--png" in command:
            for size in icons.ANDROID_ICON_SIZES.values():
                (output / f"{size}x{size}.png").write_bytes(f"legacy-{size}".encode())
        else:
            manifest = json.loads(Path(command[3]).read_text())
            self.assertEqual(manifest["default"], str(self.desktop / "icon.svg"))
            self.assertEqual(manifest["bg_color"], "#000000")
            foreground = icons.ET.parse(manifest["android_fg"]).getroot()
            monochrome = icons.ET.parse(manifest["android_monochrome"]).getroot()
            self.assertEqual(foreground.get("viewBox"), "-5 5 90 90")
            self.assertEqual(monochrome.get("color"), "#FFFFFF")
            self.assertEqual(len(monochrome.find(f"{icons.SVG}g")), 1)
            for filename in LAUNCHER_FILES:
                path = output / "android" / filename
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(f"adaptive-{filename}".encode())
            for filename in ("icon.png", "icon.ico"):
                (output / filename).write_bytes(f"desktop-{filename}".encode())
        return subprocess.CompletedProcess(command, 0)

    def invoke(self, arguments=(), render=None):
        with patch.multiple(icons, ROOT=self.root, DESKTOP_ICONS=self.desktop,
                            ANDROID_SHELL=self.phone), \
                patch.object(sys, "argv", [str(SCRIPT), *arguments]), \
                contextlib.redirect_stdout(io.StringIO()):
            if render is None:
                icons.main()
            else:
                with patch.object(icons.subprocess, "run", side_effect=render), \
                        patch.object(icons.subprocess, "check_output", return_value="tauri-cli 2.11.1\n"):
                    icons.main()

    def assert_legacy_launchers(self, resources):
        for density, size in icons.ANDROID_ICON_SIZES.items():
            for filename in ("ic_launcher.png", "ic_launcher_round.png"):
                self.assertEqual((resources / f"mipmap-{density}" / filename).read_bytes(),
                                 f"legacy-{size}".encode())

    def test_default_generation_preserves_scaffold_and_fallback_destinations(self):
        for scaffold in (False, True):
            with self.subTest(scaffold=scaffold):
                resources = self.phone / "gen/android/app/src/main/res"
                if scaffold:
                    resources.mkdir(parents=True)
                self.invoke(render=self.fake_render)
                destination = resources if scaffold else self.phone / "icons/android"
                self.assertEqual(set(file_bytes(destination)), set(LAUNCHER_FILES))
                self.assert_legacy_launchers(destination)
                self.assertEqual((self.desktop / "icon.png").read_bytes(), b"desktop-icon.png")
                self.assertEqual((self.desktop / "icon.ico").read_bytes(), b"desktop-icon.ico")
                self.assertEqual((self.phone / "icons/icon.png").read_bytes(), b"desktop-icon.png")

    def test_wear_only_outputs_resources_without_phone_scaffold_or_desktop_writes(self):
        destination = self.root / "wear build/generated/launcher-res"
        before = file_bytes(self.desktop)
        self.invoke(["--wear-res-output", str(destination)], self.fake_render)
        self.assertEqual(set(file_bytes(destination)),
                          set(LAUNCHER_FILES) | {"drawable-nodpi/capture_background.png", "values/artwork_geometry.xml"})
        self.assertEqual((destination / "values/artwork_geometry.xml").read_text(),
                         icons.artwork_geometry_resource(icons.ET.fromstring(SYNTHETIC_SVG)))
        self.assert_legacy_launchers(destination)
        self.assertEqual((destination / "drawable-nodpi/capture_background.png").read_bytes(),
                         b"desktop-icon.png")
        self.assertEqual(file_bytes(self.desktop), before)
        self.assertFalse(self.phone.exists())
        self.assertFalse((self.root / "target").exists())
        self.assertEqual(len(self.calls), 2)

    def test_canonical_ink_fraction_uses_outer_stroke_and_keeps_inner_black(self):
        source = icons.ET.parse(CANONICAL_SVG).getroot()
        before = icons.ET.tostring(source)
        self.assertAlmostEqual(icons.artwork_geometry(source), 62 / 68.888889, places=9)
        self.assertEqual(icons.ET.tostring(source), before)
        self.assertIsNotNone(source.find(f"{icons.SVG}g/{icons.SVG}circle[@fill='#000000']"))

    def test_geometry_tracks_changed_canvas_and_inherited_outer_stroke(self):
        source = icons.ET.fromstring('''<svg xmlns="http://www.w3.org/2000/svg" viewBox="10 20 80 80">
          <g id="mark" fill="none" stroke="currentColor" stroke-width="8">
            <circle cx="50" cy="60" r="30"/>
          </g></svg>''')
        self.assertAlmostEqual(icons.artwork_geometry(source), 68 / 80)
        source.find(f"{icons.SVG}g").set("stroke-width", "12")
        self.assertAlmostEqual(icons.artwork_geometry(source), 72 / 80)
        self.assertIn("90.000000000%", icons.artwork_geometry_resource(source))

    def test_unknown_or_clipped_geometry_fails_instead_of_using_a_watch_zoom(self):
        for attribute, value in (("cx", "41"), ("r", "31"), ("r", "nan"), ("transform", "scale(2)")):
            with self.subTest(attribute=attribute):
                source = icons.ET.fromstring(SYNTHETIC_SVG)
                source.find(f"{icons.SVG}g/{icons.SVG}circle").set(attribute, value)
                with self.assertRaises(ValueError):
                    icons.artwork_geometry(source)
        source = icons.ET.fromstring(SYNTHETIC_SVG)
        source.set("viewBox", "10 20 60 80")
        with self.assertRaises(ValueError):
            icons.artwork_geometry(source)

    def test_help_does_not_require_renderer(self):
        with patch.object(sys, "argv", [str(SCRIPT), "--help"]), \
                patch.object(icons.subprocess, "check_output") as version, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            with self.assertRaises(SystemExit) as error:
                icons.main()
        self.assertEqual(error.exception.code, 0)
        self.assertIn("--wear-res-output", output.getvalue())
        self.assertIn("--phone-shortcut-res-output", output.getvalue())
        version.assert_not_called()

    def test_resource_output_modes_are_mutually_exclusive(self):
        with patch.object(sys, "argv", [str(SCRIPT), "--wear-res-output", "wear",
                                        "--phone-shortcut-res-output", "phone"]), \
                patch.object(icons.subprocess, "check_output") as version, \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            with self.assertRaises(SystemExit) as error:
                icons.main()
        self.assertEqual(error.exception.code, 2)
        self.assertIn("not allowed with argument", errors.getvalue())
        version.assert_not_called()

    def test_shortcut_derives_mark_and_badge_without_mutating_source(self):
        source = icons.ET.fromstring(SYNTHETIC_SVG)
        mark = source.find(f"{icons.SVG}g[@id='mark']")
        mark.set("transform", "rotate(10 54 54)")
        before = icons.ET.tostring(source)
        derived = icons.phone_shortcut_layer(source)
        self.assertEqual(icons.ET.tostring(source), before)
        self.assertEqual(derived.get("viewBox"), source.get("viewBox"))
        scaled = derived.find(f"{icons.SVG}g[@id='shortcut-mark']")
        self.assertEqual(scaled.get("transform"), "translate(54 54) scale(0.82) translate(-54 -54)")
        self.assertEqual(icons.ET.tostring(scaled.find(f"{icons.SVG}g[@id='mark']")),
                         icons.ET.tostring(mark))
        badge = derived.find(f"{icons.SVG}g[@id='new-voice-badge']")
        circle = badge.find(f"{icons.SVG}circle")
        self.assertEqual(circle.attrib, {"cx": "72", "cy": "72", "r": "8",
                                         "fill": "#000000", "stroke": "none"})
        plus = badge.find(f"{icons.SVG}path")
        self.assertEqual(plus.get("d"), "M68 72H76M72 68V76")
        self.assertEqual(plus.get("stroke"), "#12ABCD")
        self.assertEqual(plus.get("stroke-width"), "3")
        self.assertEqual(plus.get("stroke-linecap"), "round")
        self.assertLess(math.hypot(72 - 54, 72 - 54) + 8, 34.4444445)
        self.assertLess(0.82 * (28 + 6 / 2), 34.4444445)

    def test_shortcut_requires_concrete_source_color_and_mark(self):
        for color in (None, "", "currentColor", "url(https://invalid.example/color)", "#NOTHEX"):
            with self.subTest(color=color):
                source = icons.ET.fromstring(SYNTHETIC_SVG)
                if color is None:
                    del source.attrib["color"]
                else:
                    source.set("color", color)
                with self.assertRaisesRegex(ValueError, "root color"):
                    icons.phone_shortcut_layer(source)
        source = icons.ET.fromstring(SYNTHETIC_SVG)
        source.remove(source.find(f"{icons.SVG}g"))
        with self.assertRaisesRegex(ValueError, "mark group"):
            icons.phone_shortcut_layer(source)

    def test_shortcut_only_publishes_named_png_from_temporary_derived_svg(self):
        destination = self.root / "phone build/generated/shortcut-res"
        before = file_bytes(self.root)

        def render_shortcut(command, **kwargs):
            self.calls.append(command)
            derived_path = Path(command[3])
            self.assertNotEqual(derived_path, self.desktop / "icon.svg")
            derived = icons.ET.parse(derived_path).getroot()
            self.assertEqual(derived.get("color"), "#12ABCD")
            self.assertIsNotNone(derived.find(f"{icons.SVG}g[@id='new-voice-badge']"))
            output = Path(command[command.index("--output") + 1])
            output.mkdir()
            (output / "icon.png").write_bytes(b"synthetic-shortcut-png")
            return subprocess.CompletedProcess(command, 0)

        self.invoke(["--phone-shortcut-res-output", str(destination)], render_shortcut)
        self.assertEqual(file_bytes(destination), {
            "drawable-nodpi/voxgolem_new_voice_shortcut.png": b"synthetic-shortcut-png",
        })
        self.assertEqual({name: data for name, data in file_bytes(self.root).items()
                          if not name.startswith("phone build/")}, before)
        self.assertFalse((self.root / "target").exists())
        self.assertEqual(len(self.calls), 1)
        self.assertFalse(Path(self.calls[0][3]).exists())

    def test_shortcut_cli_invalid_color_fails_clearly_without_publishing(self):
        (self.desktop / "icon.svg").write_text(SYNTHETIC_SVG.replace('color="#12ABCD"', 'color="currentColor"'))
        destination = self.root / "shortcut-res"
        with contextlib.redirect_stderr(io.StringIO()) as errors:
            with self.assertRaises(SystemExit) as error:
                self.invoke(["--phone-shortcut-res-output", str(destination)], self.fake_render)
        self.assertEqual(error.exception.code, 2)
        self.assertIn("root color", errors.getvalue())
        self.assertFalse(destination.exists())
        self.assertEqual(self.calls, [])

    def test_output_modes_require_pinned_renderer_before_creating_output(self):
        destination = self.root / "generated-res"
        for mode in ("--wear-res-output", "--phone-shortcut-res-output"):
            with self.subTest(mode=mode), \
                    patch.object(sys, "argv", [str(SCRIPT), mode, str(destination)]), \
                    patch.object(icons.subprocess, "check_output", return_value="tauri-cli 0.0.0\n"), \
                    patch.object(icons.subprocess, "run") as render:
                with self.assertRaisesRegex(SystemExit, "requires tauri-cli 2.11.1"):
                    icons.main()
                render.assert_not_called()
                self.assertFalse(destination.exists())

    def test_render_failure_does_not_publish_or_touch_other_outputs(self):
        destination = self.root / "generated-res"
        before = file_bytes(self.root)

        def fail_render(command, **kwargs):
            raise subprocess.CalledProcessError(1, command)

        for mode in ("--wear-res-output", "--phone-shortcut-res-output"):
            with self.subTest(mode=mode), self.assertRaises(subprocess.CalledProcessError):
                self.invoke([mode, str(destination)], fail_render)
            self.assertEqual(file_bytes(self.root), before)
            self.assertFalse(destination.exists())

    @unittest.skipUnless(shutil.which("cargo"), "requires existing pinned cargo tauri renderer")
    def test_real_renderer_outputs_are_byte_identical_between_default_and_wear(self):
        shutil.copyfile(CANONICAL_SVG, self.desktop / "icon.svg")
        self.invoke()
        expected = file_bytes(self.phone / "icons/android")
        expected["drawable-nodpi/capture_background.png"] = (self.phone / "icons/icon.png").read_bytes()
        expected["values/artwork_geometry.xml"] = icons.artwork_geometry_resource(icons.ET.parse(CANONICAL_SVG).getroot()).encode()
        digest = hashlib.sha256()
        for filename, data in sorted(expected.items()):
            digest.update(filename.encode() + b"\0" + data)
        print(f"Default renderer control: {len(expected)} Wear files, sha256={digest.hexdigest()}")
        before = file_bytes(self.root)
        destination = self.root / "wear build/generated/launcher-res"
        self.invoke(["--wear-res-output", str(destination)])
        self.assertEqual(set(file_bytes(destination)), set(expected))
        for filename, data in expected.items():
            with self.subTest(filename=filename):
                self.assertEqual((destination / filename).read_bytes(), data)
        self.assertEqual({name: data for name, data in file_bytes(self.root).items()
                          if not name.startswith("wear build/")}, before)
        authored_directories = (
            CANONICAL_SVG.parent, icons.ANDROID_SHELL / "icons",
            icons.ANDROID_SHELL / "gen/android/app/src/main/res",
        )
        authored_before = [file_bytes(directory) for directory in authored_directories]
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--wear-res-output", "wear build/generated/launcher-res"],
            cwd=self.root, check=True, capture_output=True, text=True,
        )
        self.assertIn("Generated Wear resources", result.stdout)
        self.assertEqual(file_bytes(destination), expected)
        self.assertEqual([file_bytes(directory) for directory in authored_directories], authored_before)

    @unittest.skipUnless(shutil.which("cargo") and shutil.which("ffmpeg"),
                         "requires existing cargo tauri renderer and ffmpeg pixel decoder")
    def test_real_shortcut_yellow_badge_stays_inside_circle_and_preserves_normal_outputs(self):
        shutil.copyfile(CANONICAL_SVG, self.desktop / "icon.svg")
        self.invoke()
        before = file_bytes(self.root)
        destination = self.root / "shortcut build/generated/res"
        self.invoke(["--phone-shortcut-res-output", str(destination)])
        files = file_bytes(destination)
        self.assertEqual(set(files), {"drawable-nodpi/voxgolem_new_voice_shortcut.png"})
        self.assertEqual({name: data for name, data in file_bytes(self.root).items()
                          if not name.startswith("shortcut build/")}, before)
        authored_directories = (
            CANONICAL_SVG.parent, icons.ANDROID_SHELL / "icons",
            icons.ANDROID_SHELL / "gen/android/app/src/main/res",
        )
        authored_before = [file_bytes(directory) for directory in authored_directories]
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--phone-shortcut-res-output", "shortcut build/generated/res"],
            cwd=self.root, check=True, capture_output=True, text=True,
        )
        self.assertIn("Generated phone shortcut resource", result.stdout)
        self.assertEqual(file_bytes(destination), files)
        self.assertEqual([file_bytes(directory) for directory in authored_directories], authored_before)
        png = destination / "drawable-nodpi/voxgolem_new_voice_shortcut.png"
        width, height = struct.unpack(">II", png.read_bytes()[16:24])
        self.assertEqual((width, height), (512, 512))
        pixels = subprocess.check_output([
            "ffmpeg", "-v", "error", "-i", str(png), "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1",
        ])
        self.assertEqual(len(pixels), width * height * 4)
        source = icons.ET.parse(self.desktop / "icon.svg").getroot()
        color = source.get("color").lstrip("#")
        yellow = bytes.fromhex(color) + b"\xff"
        x, y, view_width, view_height = map(float, source.get("viewBox").split())

        def pixel_at(svg_x, svg_y):
            px = int((svg_x - x) * width / view_width)
            py = int((svg_y - y) * height / view_height)
            offset = (py * width + px) * 4
            return pixels[offset:offset + 4]

        for point in ((72, 72), (69, 72), (75, 72), (72, 69), (72, 75)):
            self.assertEqual(pixel_at(*point), yellow)
        self.assertEqual(pixel_at(75, 75), b"\x00\x00\x00\xff")
        max_radius_squared = max(
            (index % width + 0.5 - width / 2) ** 2 + (index // width + 0.5 - height / 2) ** 2
            for index in range(width * height) if pixels[index * 4 + 3]
        )
        self.assertLessEqual(max_radius_squared, (width / 2) ** 2)
        print(f"Shortcut PNG: sha256={hashlib.sha256(png.read_bytes()).hexdigest()}, "
              f"nontransparent radius={math.sqrt(max_radius_squared) * view_width / width:.4f} SVG units")
        self.invoke()
        self.assertEqual({name: data for name, data in file_bytes(self.root).items()
                          if not name.startswith("shortcut build/")}, before)


if __name__ == "__main__":
    unittest.main()
