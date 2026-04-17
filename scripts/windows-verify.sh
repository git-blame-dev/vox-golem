#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ -d /mnt/c/WINDOWS/system32 ]; then
  PATH="$PATH:/mnt/c/WINDOWS/system32"
fi

if [ -d /mnt/c/Windows/System32 ]; then
  PATH="$PATH:/mnt/c/Windows/System32"
fi

export PATH

if ! command -v cmd.exe >/dev/null 2>&1; then
  printf 'Unable to find cmd.exe in the current WSL environment.\n' >&2
  exit 1
fi

if command -v wslpath >/dev/null 2>&1; then
  cmd_path=$(wslpath -w "$script_dir/windows-verify.cmd")
else
  unix_path="$script_dir/windows-verify.cmd"

  case "$unix_path" in
    /mnt/[A-Za-z]/*)
      drive_letter=$(printf '%s' "$unix_path" | cut -d/ -f3 | tr '[:lower:]' '[:upper:]')
      path_without_drive=$(printf '%s' "$unix_path" | cut -d/ -f4- | sed 's,/,\\,g')
      cmd_path="${drive_letter}:\\${path_without_drive}"
      ;;
    *)
      printf 'Unable to convert path for cmd.exe: %s\n' "$unix_path" >&2
      exit 1
      ;;
  esac
fi

cmd.exe /c "$cmd_path"
