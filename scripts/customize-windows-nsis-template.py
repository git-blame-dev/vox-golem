#!/usr/bin/env python3
import pathlib
import sys


def replace_once(contents: str, old: str, new: str) -> str:
    if contents.count(old) != 1:
        raise SystemExit("Tauri NSIS template no longer matches the pinned customization")
    return contents.replace(old, new)


source = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
contents = source.read_text(encoding="utf-8")
contents = replace_once(
    contents,
    '''; 5. Choose install directory page
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_DIRECTORY
''',
    "; VoxGolem always installs to its canonical per-user directory.\n",
)
contents = replace_once(
    contents,
    '''; Use show readme button in the finish page as a button create a desktop shortcut
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "$(createDesktop)"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateOrUpdateDesktopShortcut
''',
    "; VoxGolem intentionally creates only a Start Menu shortcut.\n",
)
contents = replace_once(
    contents,
    '''  ; Create desktop shortcut for silent and passive installers
  ; because finish page will be skipped
  ${If} $PassiveMode = 1
  ${OrIf} ${Silent}
    Call CreateOrUpdateDesktopShortcut
  ${EndIf}
''',
    "  ; Desktop shortcuts are intentionally disabled for every install mode.\n",
)
contents = replace_once(
    contents,
    r'''  ${If} $INSTDIR == "${PLACEHOLDER_INSTALL_DIR}"
    ; Set default install location
    !if "${INSTALLMODE}" == "perMachine"
      ${If} ${RunningX64}
        !if "${ARCH}" == "x64"
          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
        !else if "${ARCH}" == "arm64"
          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
        !else
          StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
        !endif
      ${Else}
        StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
      ${EndIf}
    !else if "${INSTALLMODE}" == "currentUser"
      StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"
    !endif

    Call RestorePreviousInstallLocation
  ${EndIf}
''',
    r'''  ; Ignore /D and historical registry locations so every mode has one identity.
  StrCpy $INSTDIR "$LOCALAPPDATA\Programs\VoxGolem"
''',
)
destination.parent.mkdir(parents=True, exist_ok=True)
destination.write_text(contents, encoding="utf-8")
