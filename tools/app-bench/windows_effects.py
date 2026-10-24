"""Windows-only effects for the shared offline voice diagnostic (stdlib + Selenium).

No namespace/network/hardware isolation claim. Win32 structures/signatures follow
Microsoft Learn: CreateProcessW, JOBOBJECT_{BASIC,EXTENDED}_LIMIT_INFORMATION,
JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, IsProcessInJob and GetProcessTimes.
No breakaway flags, inherited handles, global process-name termination or install.
"""
import ctypes as C
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import struct
import subprocess
import sys
import time
import tomllib

# Fixed-width Windows ABI types also make layout tests meaningful on Linux.
DWORD = C.c_uint32
WORD = C.c_uint16
BOOL = C.c_int32
HANDLE = C.c_void_p
SIZE_T = C.c_size_t
QWORD = C.c_uint64


class BasicLimits(C.Structure):
    _fields_ = [('PerProcessUserTimeLimit', C.c_int64), ('PerJobUserTimeLimit', C.c_int64),
                ('LimitFlags', DWORD), ('MinimumWorkingSetSize', SIZE_T),
                ('MaximumWorkingSetSize', SIZE_T), ('ActiveProcessLimit', DWORD),
                ('Affinity', SIZE_T), ('PriorityClass', DWORD), ('SchedulingClass', DWORD)]


class IoCounters(C.Structure):
    _fields_ = [(name, QWORD) for name in ('ReadOperationCount', 'WriteOperationCount',
        'OtherOperationCount', 'ReadTransferCount', 'WriteTransferCount', 'OtherTransferCount')]


class ExtendedLimits(C.Structure):
    _fields_ = [('BasicLimitInformation', BasicLimits), ('IoInfo', IoCounters),
                ('ProcessMemoryLimit', SIZE_T), ('JobMemoryLimit', SIZE_T),
                ('PeakProcessMemoryUsed', SIZE_T), ('PeakJobMemoryUsed', SIZE_T)]


class Accounting(C.Structure):
    _fields_ = [(name, C.c_int64) for name in ('TotalUserTime', 'TotalKernelTime',
        'ThisPeriodTotalUserTime', 'ThisPeriodTotalKernelTime')] + [
        (name, DWORD) for name in ('TotalPageFaultCount', 'TotalProcesses',
                                 'ActiveProcesses', 'TotalTerminatedProcesses')]


class StartupInfo(C.Structure):
    _fields_ = [('cb', DWORD), ('lpReserved', C.c_wchar_p), ('lpDesktop', C.c_wchar_p),
        ('lpTitle', C.c_wchar_p)] + [(name, DWORD) for name in ('dwX', 'dwY', 'dwXSize',
        'dwYSize', 'dwXCountChars', 'dwYCountChars', 'dwFillAttribute', 'dwFlags')] + [
        ('wShowWindow', WORD), ('cbReserved2', WORD), ('lpReserved2', C.c_void_p),
        ('hStdInput', HANDLE), ('hStdOutput', HANDLE), ('hStdError', HANDLE)]


class ProcessInfo(C.Structure):
    _fields_ = [('hProcess', HANDLE), ('hThread', HANDLE), ('dwProcessId', DWORD), ('dwThreadId', DWORD)]


class FileTime(C.Structure):
    _fields_ = [('low', DWORD), ('high', DWORD)]


def validate_options(args, host=None):
    is_win32 = (sys.platform if host is None else host) == 'win32' or getattr(args, '_test_win32', False)
    if not is_win32 or getattr(args, 'live', False) or getattr(args, 'entry', 'voice-arithmetic') != 'voice-arithmetic':
        raise ValueError('Windows supports only native offline arithmetic diagnostics')
    if getattr(args, 'windows_manifest', None) is None:
        raise ValueError('Windows requires a verified staged fixture/asset manifest')


def session_creation_cause(error):
    """Classify explicit driver statements only; never return exception content."""
    if type(error).__name__ != 'SessionNotCreatedException':
        return 'unknown'
    message = getattr(error, 'msg', None)
    if not isinstance(message, str) or len(message) > 4096:
        return 'unknown'
    patterns = {
        'version_mismatch': r'(?:session not created: )?this version of microsoft edge webdriver only supports microsoft edge version [0-9]+',
        'unsupported_capability': r'(?:session not created: )?(?:invalid argument: )?unrecognized capability: [a-z0-9:._-]+',
        'application_launch_failed': r'(?:session not created: )?microsoft edge failed to start: exited abnormally\.',
        'webview_attach_timeout': r'(?:session not created: )?timed out waiting for webview2 to connect',
    }
    causes = {cause for line in message.lower().splitlines() for cause, pattern in patterns.items()
              if re.fullmatch(pattern, line.strip())}
    return next(iter(causes)) if len(causes) == 1 else 'unknown'


def reject_reparse(path):
    if sys.platform == 'win32' and str(path).startswith('\\\\'):
        raise ValueError('Windows diagnostics require local drive storage, not UNC paths')
    for part in (path, *path.parents):
        if part.exists() and (part.is_symlink() or getattr(part.lstat(), 'st_file_attributes', 0) & 0x400):
            raise ValueError('Reparse paths are not permitted')


def bundle_path(root, name):
    if (not isinstance(name, str) or not name or len(name) > 1024 or '\\' in name or ':' in name or
            any(part in ('', '.', '..') for part in name.split('/')) or PurePosixPath(name).is_absolute()):
        raise ValueError('Manifest path is not bundle-relative')
    if any(part.endswith((' ', '.')) for part in name.split('/')):
        raise ValueError('Windows path normalization aliases are not permitted')
    path = root.joinpath(*name.split('/'))
    reject_reparse(path)
    if not path.resolve().is_relative_to(root.resolve()):
        raise ValueError('Manifest path escapes bundle')
    return path


def file_hash(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        while block := stream.read(1024*1024):
            digest.update(block)
    return digest.hexdigest()


def validate_pe(path):
    with path.open('rb') as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:2] != b'MZ':
            raise ValueError('Windows executable lacks a DOS header')
        offset = struct.unpack_from('<I', header, 60)[0]
        if not 64 <= offset <= 1024*1024:
            raise ValueError('PE header offset invalid')
        stream.seek(offset)
        pe = stream.read(26)
        if (len(pe) != 26 or pe[:4] != b'PE\0\0' or
                struct.unpack_from('<H', pe, 4)[0] != 0x8664 or
                struct.unpack_from('<H', pe, 24)[0] != 0x20b):
            raise ValueError('Windows x64 PE32+ executable required')


def application_capability(binary):
    # Same Selenium/Tauri protocol as Linux, not a second WebDriver client.
    return {'application': str(binary.resolve())}


def powershell(script, extra=None):
    if sys.platform != 'win32':
        raise ValueError('Windows-native effects require Windows')
    executable = Path(os.environ['SystemRoot'])/'System32/WindowsPowerShell/v1.0/powershell.exe'
    env = dict(os.environ, **(extra or {}))
    completed = subprocess.run([str(executable), '-NoLogo', '-NoProfile', '-NonInteractive',
        '-Command', "$ErrorActionPreference='Stop';"+script], env=env,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30)
    if completed.returncode or len(completed.stdout) > 4096:
        raise ValueError('Windows native prerequisite verification failed')
    return completed.stdout.decode('utf-8-sig').strip()


# Directory.CreateDirectory(path, DirectorySecurity) creates with a protected DACL,
# not a chmod approximation or a create-then-restrict window. Verify owner and the
# exact SID/full-control/inheritance rules through .NET before using the directory.
ACL_SCRIPT = r'''
$p=$env:VOXGOLEM_BENCH_ACL_PATH
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User
$system=New-Object Security.Principal.SecurityIdentifier('S-1-5-18')
if($env:VOXGOLEM_BENCH_ACL_CREATE -eq '1') {
  if(Test-Path -LiteralPath $p){throw 'exists'}
  $acl=New-Object Security.AccessControl.DirectorySecurity
  $acl.SetOwner($sid);$acl.SetAccessRuleProtection($true,$false)
  foreach($who in @($sid,$system)) {
    $rule=New-Object Security.AccessControl.FileSystemAccessRule($who,'FullControl','ContainerInherit,ObjectInherit','None','Allow')
    $acl.AddAccessRule($rule)
  }
  [IO.Directory]::CreateDirectory($p,$acl) | Out-Null
}
$got=[IO.Directory]::GetAccessControl($p)
if(-not $got.AreAccessRulesProtected -or $got.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid.Value){throw 'owner'}
$rules=@($got.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]))
if($rules.Count -ne 2){throw 'rules'}
$seen=@{}
foreach($r in $rules){
  if($r.IsInherited -or $r.AccessControlType -ne 'Allow' -or $r.FileSystemRights -ne 'FullControl' -or
     $r.InheritanceFlags -ne 'ContainerInherit,ObjectInherit' -or $r.PropagationFlags -ne 'None' -or
     $r.IdentityReference.Value -notin @($sid.Value,$system.Value)){throw 'rule'}
  $seen[$r.IdentityReference.Value]=$true
}
if($seen.Count -ne 2){throw 'duplicate'}
Write-Output 'private'
'''


def private_directory(path, create=True):
    reject_reparse(path)
    if not path.parent.is_dir() or (create and path.exists()):
        raise ValueError('Private directory requires a new path and existing parent')
    if powershell(ACL_SCRIPT, {'VOXGOLEM_BENCH_ACL_PATH': str(path.resolve()),
            'VOXGOLEM_BENCH_ACL_CREATE': '1' if create else '0'}) != 'private':
        raise ValueError('Private directory ACL was not verified')
    reject_reparse(path)


def verify_native_driver_version(path, expected):
    version = powershell(r'''
$v=[Diagnostics.FileVersionInfo]::GetVersionInfo($env:VOXGOLEM_BENCH_DRIVER_FILE).ProductVersion
if($v -notmatch '^\d+\.\d+\.\d+\.\d+$'){throw 'version'}
Write-Output $v
''', {'VOXGOLEM_BENCH_DRIVER_FILE': str(path.resolve())})
    if version != expected:
        raise ValueError('Native EdgeDriver file version does not match staging identity')
    return version


class WinApi:
    def __init__(self):
        if sys.platform != 'win32' or C.sizeof(C.c_void_p) != 8:
            raise ValueError('Windows x64 native controller required')
        self.k = C.WinDLL('kernel32', use_last_error=True)
        signatures = {
            'CreateJobObjectW': ([C.c_void_p, C.c_wchar_p], HANDLE),
            'SetInformationJobObject': ([HANDLE, C.c_int, C.c_void_p, DWORD], BOOL),
            'QueryInformationJobObject': ([HANDLE, C.c_int, C.c_void_p, DWORD, C.c_void_p], BOOL),
            'AssignProcessToJobObject': ([HANDLE, HANDLE], BOOL),
            'IsProcessInJob': ([HANDLE, HANDLE, C.POINTER(BOOL)], BOOL),
            'CreateProcessW': ([C.c_wchar_p, C.c_wchar_p, C.c_void_p, C.c_void_p, BOOL,
                DWORD, C.c_void_p, C.c_wchar_p, C.POINTER(StartupInfo), C.POINTER(ProcessInfo)], BOOL),
            'ResumeThread': ([HANDLE], DWORD), 'CloseHandle': ([HANDLE], BOOL),
            'TerminateProcess': ([HANDLE, C.c_uint], BOOL),
            'TerminateJobObject': ([HANDLE, C.c_uint], BOOL),
            'WaitForSingleObject': ([HANDLE, DWORD], DWORD),
            'OpenProcess': ([DWORD, BOOL, DWORD], HANDLE),
            'QueryFullProcessImageNameW': ([HANDLE, DWORD, C.c_wchar_p, C.POINTER(DWORD)], BOOL),
            'GetProcessTimes': ([HANDLE]+[C.POINTER(FileTime)]*4, BOOL),
            'CreateMutexW': ([C.c_void_p, BOOL, C.c_wchar_p], HANDLE),
        }
        for name, (args, result) in signatures.items():
            function = getattr(self.k, name)
            function.argtypes, function.restype = args, result

    def checked(self, result):
        if not result:
            error = OSError('Windows process ownership operation failed')
            error.native_error_code = C.get_last_error()
            raise error
        return result

    def close(self, handle):
        self.checked(self.k.CloseHandle(handle))

    def new_job(self):
        job = self.checked(self.k.CreateJobObjectW(None, None))
        limits = ExtendedLimits()
        limits.BasicLimitInformation.LimitFlags = 0x2000  # KILL_ON_JOB_CLOSE; never BREAKAWAY
        try:
            self.checked(self.k.SetInformationJobObject(job, 9, C.byref(limits), C.sizeof(limits)))
        except BaseException:
            self.close(job)
            raise
        return job

    def controller_mutex(self):
        C.set_last_error(0)
        handle = self.checked(self.k.CreateMutexW(None, False, r'Local\VoxGolemArithmeticBenchmarkController'))
        if C.get_last_error() == 183:
            self.close(handle)
            raise ValueError('Another benchmark controller already exists')
        return handle

    def create_suspended(self, command, environment, cwd):
        si, pi = StartupInfo(), ProcessInfo()
        si.cb = C.sizeof(si)
        if any('\0' in key or '=' in key or '\0' in value for key, value in environment.items()):
            raise ValueError('Invalid Windows environment block')
        block = C.create_unicode_buffer('\0'.join(f'{k}={v}' for k, v in
            sorted(environment.items(), key=lambda pair: pair[0].upper()))+'\0\0')
        line = C.create_unicode_buffer(subprocess.list2cmdline(command))
        self.checked(self.k.CreateProcessW(command[0], line, None, None, False,
            0x4 | 0x400 | 0x08000000, block, str(cwd), C.byref(si), C.byref(pi)))
        return pi.hProcess, pi.hThread

    def assign(self, job, process):
        self.checked(self.k.AssignProcessToJobObject(job, process))

    def resume(self, thread):
        if self.k.ResumeThread(thread) == 0xffffffff:
            raise OSError('Windows suspended child could not resume')

    def terminate_process(self, process):
        self.checked(self.k.TerminateProcess(process, 2))
        if not self.wait(process, 10):
            raise OSError('Suspended child did not retire')

    def wait(self, handle, seconds):
        value = self.k.WaitForSingleObject(handle, int(seconds*1000))
        if value not in (0, 258):
            raise OSError('Windows process wait failed')
        return value == 0

    def in_job(self, process, job):
        result = BOOL()
        self.checked(self.k.IsProcessInJob(process, job, C.byref(result)))
        return bool(result.value)

    def open_process(self, pid):
        return self.checked(self.k.OpenProcess(0x1000 | 0x100000, False, pid))

    def image_and_created(self, process):
        name, length = C.create_unicode_buffer(32768), DWORD(32768)
        self.checked(self.k.QueryFullProcessImageNameW(process, 0, name, C.byref(length)))
        times = [FileTime() for _ in range(4)]
        self.checked(self.k.GetProcessTimes(process, *(C.byref(value) for value in times)))
        return Path(name.value).resolve(), times[0].low | (times[0].high << 32)

    def active(self, job):
        value = Accounting()
        self.checked(self.k.QueryInformationJobObject(job, 1, C.byref(value), C.sizeof(value), None))
        return value.ActiveProcesses

    def terminate_job(self, job):
        self.checked(self.k.TerminateJobObject(job, 2))


def start_assigned(api, job, command, environment, cwd):
    process, thread = api.create_suspended(command, environment, cwd)
    try:
        api.assign(job, process)
        api.resume(thread)
    except BaseException:
        try:
            api.terminate_process(process)  # Exact suspended owned handle, never PID/name.
        finally:
            api.close(process)
        raise
    finally:
        api.close(thread)
    return process


class WindowsJob:
    def __init__(self, api=None):
        self.api = api or WinApi()
        self.mutex = self.api.controller_mutex()
        try:
            self.job = self.api.new_job()
        except BaseException:
            self.api.close(self.mutex)
            raise
        self.driver = self.app = None
        self.created = None

    def start(self, command, env, cwd):
        self.driver = start_assigned(self.api, self.job, command, env, cwd)

    def observe_app(self, pid, binary):
        if self.app is not None:
            return
        handle = self.api.open_process(pid)
        try:
            if not self.api.in_job(handle, self.job):
                raise ValueError('App is not a member of the owned driver job')
            image, created = self.api.image_and_created(handle)
            if image != binary.resolve() or created <= 0 or self.api.wait(handle, 0):
                raise ValueError('App executable/creation identity is not owned')
            self.app, self.created = handle, created
        except BaseException:
            self.api.close(handle)
            raise

    def shutdown(self, driver):
        quit_error = False
        graceful = retired = forced = False
        tracking_failed = False
        quit_skipped = driver is not None and self.app is None
        try:
            # A session response is not app ownership. Never send Quit to an
            # unverified target: terminate only the explicitly owned job instead.
            if driver is not None and self.app is not None:
                driver.quit()
        except Exception:
            quit_error = True
        try:
            graceful = self.app is not None and self.api.wait(self.app, 3)
            forced = self.api.active(self.job) != 0
            if forced:
                self.api.terminate_job(self.job)
            deadline = time.monotonic()+10
            while self.api.active(self.job) and time.monotonic() < deadline:
                time.sleep(0.05)
            retired = self.api.active(self.job) == 0
        except Exception:
            tracking_failed = True
        # No profile deletion: preserve private artifacts. Closing the last job
        # handle remains the controller-crash fail-safe; no foreign-process kill.
        for name in ('app', 'driver', 'job', 'mutex'):
            handle = getattr(self, name)
            if handle is not None:
                try:
                    self.api.close(handle)
                except Exception:
                    tracking_failed = True
                setattr(self, name, None)
        return {'app_exit_observed': graceful, 'app_exit_before_group_termination_observed': graceful,
                'forced_group_termination': forced, 'webdriver_quit_error': quit_error,
                'webdriver_quit_skipped_unowned_app': quit_skipped,
                'owned_job_retired': retired and not tracking_failed, 'ownership_tracking_failed': tracking_failed,
                'private_profiles_preserved': True,
                'breakaway_processes': 'unverified'}


def load_manifest(path, profile):
    reject_reparse(path)
    if not path.is_file() or path.stat().st_size > 256*1024:
        raise ValueError('Bounded Windows staging manifest required')
    value = json.loads(path.read_text(encoding='utf-8'))
    if (value.get('schema') != 1 or value.get('contract_sha256') != profile.contract_sha256 or
            value.get('application_id') != 'dev.gitblame.voxgolem.benchmark' or
            not re.fullmatch('[0-9a-f]{64}', value.get('native_source_sha256', '')) or
            not isinstance(value.get('files'), dict) or not 1 <= len(value['files']) <= 4096):
        raise ValueError('Windows staging identity invalid')
    versions = value.get('driver_versions', {})
    if (set(versions) != {'tauri', 'edge', 'webview'} or
            versions['tauri'] != '2.1.0' or
            any(not isinstance(versions[key], str) or not re.fullmatch(r'\d+\.\d+\.\d+\.\d+', versions[key])
                for key in ('edge', 'webview')) or
            versions['edge'].split('.')[:3] != versions['webview'].split('.')[:3]):
        raise ValueError('Pinned Tauri and matching Edge/WebView versions required')
    root = path.resolve().parent
    files = {}
    for name, digest in value['files'].items():
        target = bundle_path(root, name)
        if not re.fullmatch('[0-9a-f]{64}', digest) or not target.is_file() or file_hash(target) != digest:
            raise ValueError('Staged artifact hash mismatch')
        files[name] = target
    roles = value.get('roles', {})
    required = {'binary', 'tauri_driver', 'native_driver', 'config', 'fixture',
                'wake_word_model_path', 'silero_vad_model', 'parakeet_model_dir',
                'tts_model', 'espeak_data_dir'}
    if set(roles) != required:
        raise ValueError('Windows manifest roles incomplete')
    resolved = {}
    for role, name in roles.items():
        target = bundle_path(root, name)
        if role in ('parakeet_model_dir', 'espeak_data_dir'):
            if not target.is_dir():
                raise ValueError('Required speech directory missing')
            actual = {file.relative_to(root).as_posix() for file in target.rglob('*') if file.is_file()}
            if not actual or not actual <= set(files):
                raise ValueError('Speech directory contains undeclared assets')
            for file in target.rglob('*'):
                reject_reparse(file)
        elif name not in files:
            raise ValueError('Role is not hash-verified')
        resolved[role] = target
    # Every file alongside the executable/drivers may affect loading. No unlisted
    # DLL/config/model files; private Python venv belongs outside those directories.
    for folder in {resolved[key].parent for key in ('binary', 'tauri_driver', 'native_driver')}:
        for file in folder.rglob('*'):
            reject_reparse(file)
            if file.is_file() and file.relative_to(root).as_posix() not in files:
                raise ValueError('Executable directory contains undeclared files')
    for key in ('binary', 'tauri_driver', 'native_driver'):
        if resolved[key].suffix.lower() != '.exe':
            raise ValueError('Windows driver and app roles require explicit .exe paths')
        validate_pe(resolved[key])
    tts_json = Path(str(resolved['tts_model'])+'.json')
    if tts_json.relative_to(root).as_posix() not in files:
        raise ValueError('Piper configuration is not hash-verified')
    if resolved['espeak_data_dir'].name != 'espeak-ng-data':
        raise ValueError('Explicit espeak-ng-data directory required')
    if resolved['config'].stat().st_size > 64*1024 or tts_json.stat().st_size > 1024*1024:
        raise ValueError('Speech configuration exceeds staging bounds')
    config = tomllib.loads(resolved['config'].read_text(encoding='utf-8'))
    allowed = {'wake_word_model_path', 'silero_vad_model', 'parakeet_model_dir',
               'initial_silence_timeout_ms', 'silence_timeout_ms', 'wake_word_detection_threshold'}
    if not set(config) <= allowed:
        raise ValueError('Windows source config must contain only declared speech settings')
    for key in ('wake_word_model_path', 'silero_vad_model', 'parakeet_model_dir'):
        raw = Path(config[key])
        if not raw.is_absolute():
            raw = resolved['config'].parent/raw
        if raw.resolve() != resolved[key].resolve():
            raise ValueError('Speech config does not match manifest assets')
    if not 0 < resolved['fixture'].stat().st_size <= 3_840_000:
        raise ValueError('Staged PCM exceeds native bounds')
    data = resolved['fixture'].read_bytes()
    metadata = value.get('fixture_metadata', {})
    row = metadata.get('arithmetic', {})
    numeric = ('byte_count', 'sample_count', 'wake_end_sample', 'question_start_sample', 'question_end_sample')
    if (set(row) != {*numeric, 'duration_seconds', 'sha256'} or
            any(type(row[key]) is not int or row[key] < 0 for key in numeric) or
            type(row['duration_seconds']) not in (int, float) or not math.isfinite(row['duration_seconds']) or
            not 0 < row['wake_end_sample'] < row['question_start_sample'] < row['question_end_sample'] or
            row['question_end_sample'] != row['sample_count'] or
            row['duration_seconds'] != row['sample_count']/16000):
        raise ValueError('Staged PCM metadata invalid')
    if (not data or len(data) > 3_840_000 or len(data) % 4 or
            set(metadata) != {'arithmetic'} or
            metadata['arithmetic'].get('sha256') != file_hash(resolved['fixture']) or
            metadata['arithmetic'].get('byte_count') != len(data) or
            metadata['arithmetic'].get('sample_count') != len(data)//4 or
            any(not (-1 <= sample <= 1) for (sample,) in struct.iter_unpack('<f', data))):
        raise ValueError('Staged PCM contract invalid')
    return value, resolved


def refuse_preexisting(paths):
    # Read image paths only, never user command lines. Unknown/inaccessible image
    # identity for a candidate executable blocks startup rather than guessing.
    script = r'''
$targets=@($env:VOXGOLEM_BENCH_IMAGES | ConvertFrom-Json)
$names=@($targets | ForEach-Object {[IO.Path]::GetFileNameWithoutExtension($_)})
foreach($name in $names) {
  foreach($p in @(Get-Process -Name $name -ErrorAction SilentlyContinue)) {
    if($name -ne $names[0] -or -not $p.Path -or $p.Path -in $targets){throw 'preexisting'}
  }
}
Write-Output 'clear'
'''
    if powershell(script, {'VOXGOLEM_BENCH_IMAGES': json.dumps([str(path.resolve()) for path in paths])}) != 'clear':
        raise ValueError('Preexisting benchmark work prevents startup')


def stage_private_espeak(run, source, manifest):
    """Match the shipping strict Windows APPDATA resolver; inherit private ACLs.

    Copy payload only (not source ACLs/metadata), and verify each destination
    against the already-declared source identity before admitting the app.
    """
    appdata = run/'appdata'
    reject_reparse(appdata)
    target = appdata
    for folder in ('VoxGolem', 'models', 'tts', 'espeak-ng-data'):
        target = target/folder
        target.mkdir()  # All fresh; never merge/overwrite an existing profile.
    prefix = manifest['roles']['espeak_data_dir']+'/'
    expected = {name[len(prefix):]: digest for name, digest in manifest['files'].items()
                if name.startswith(prefix)}
    if not expected:
        raise ValueError('Declared eSpeak payload is empty')
    copied = {}
    for name, digest in sorted(expected.items()):
        original = bundle_path(source, name)
        destination = bundle_path(target, name)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(original, destination)
        copied[name] = file_hash(destination)
        if copied[name] != digest:
            raise ValueError('Private eSpeak copy does not match declared identity')
    actual = {path.relative_to(target).as_posix() for path in target.rglob('*') if path.is_file()}
    if actual != set(expected):
        raise ValueError('Private eSpeak copy contains undeclared files')
    return hashlib.sha256(json.dumps(copied, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


class WindowsEffects:
    """Effects injected into runner.execute; case loop and acceptance stay shared."""
    def __init__(self, args):
        validate_options(args)
        self.owner = None
        self.args = args
        self.roles = {}
        self.manifest = {}
        self.run_owned = False
        self.last_cleanup = None
        self.private_storage_verified = self.app_observed = False
        self.stage = 'windows_manifest'
        self.profile_identity = {
            'profile': 'voice-arithmetic-windows-diagnostic-v1',
            'contract_sha256': ''
        }

    def prepare(self, profile, api):
        self.profile_identity = dict(profile='voice-arithmetic-windows-diagnostic-v1',
                                     contract_sha256=profile.contract_sha256,
                                     implementation_sha256=profile.implementation_sha256)
        args = self.args
        value, roles = load_manifest(args.windows_manifest, profile)
        if (args.binary.resolve() != roles['binary'] or args.config.resolve() != roles['config'] or
                args.tts_model.resolve() != roles['tts_model'] or
                args.tools.resolve() != roles['tauri_driver'].parent):
            raise ValueError('CLI paths do not match verified staging roles')
        api.validate_benchmark_binary(roles['binary'])
        self.stage = 'windows_driver_version'
        self.native_driver_version = verify_native_driver_version(roles['native_driver'], value['driver_versions']['edge'])
        self.roles, self.manifest = roles, value
        run = args.output.resolve()
        self.stage = 'windows_private_acl'
        private_directory(run)
        api.private_json(run/'.app-bench-run.json', {'schema': 1,
            'owner': 'voxgolem-app-bench', 'created_epoch_seconds': time.time()})
        self.run_owned = True
        for folder in ('home', 'config', 'data', 'state', 'cache', 'runtime', 'temp',
                       'appdata', 'localappdata', 'webview', 'fixtures'):
            private_directory(run/folder)
        shutil.copyfile(roles['fixture'], run/'fixtures/arithmetic.pcm')
        if file_hash(run/'fixtures/arithmetic.pcm') != value['fixture_metadata']['arithmetic']['sha256']:
            raise ValueError('Copied PCM does not match the staged identity')
        # Synthetic speech-only source config has already been role/hash validated.
        self.stage = 'windows_speech_assets'
        self.espeak_copy_sha256 = stage_private_espeak(run, roles['espeak_data_dir'], value)
        speech = api.speech_configuration_identity(roles['config'])
        speech.update(web_enabled=False, tts_enabled=True)
        identity = api.arithmetic_identity(profile, roles['tts_model'], speech)
        identity.update(profile='voice-arithmetic-windows-diagnostic-v1',
            espeak_copy_sha256=self.espeak_copy_sha256,
            output_route='wasapi-render-diagnostic', os_network_containment='unverified',
            hardware_isolation='unverified', espeak_assets_sha256=hashlib.sha256(json.dumps(
                {name: digest for name, digest in value['files'].items()
                  if name.startswith(value['roles']['espeak_data_dir']+'/')}, sort_keys=True).encode()).hexdigest())
        self.profile_identity = identity
        return value['fixture_metadata'], speech, identity

    def environment(self, endpoint, api):
        self.stage = 'windows_private_environment'
        args, roles, run = self.args, self.roles, self.args.output.resolve()
        system_keys = {'SYSTEMROOT', 'WINDIR', 'SYSTEMDRIVE', 'COMSPEC', 'PROGRAMFILES',
            'PROGRAMFILES(X86)', 'PROGRAMW6432', 'PROGRAMDATA', 'NUMBER_OF_PROCESSORS',
            'PROCESSOR_ARCHITECTURE', 'PROCESSOR_IDENTIFIER', 'PROCESSOR_LEVEL', 'PROCESSOR_REVISION'}
        source = {key: value for key, value in os.environ.items() if key.upper() in system_keys}
        env = api.isolated_environment(run, roles['config'], endpoint,
            entry='voice-arithmetic', tts_model=roles['tts_model'], output_route='windows-diagnostic',
            base_environment=source)
        env.update(APPDATA=str(run/'appdata'), LOCALAPPDATA=str(run/'localappdata'),
            USERPROFILE=str(run/'home'), TEMP=str(run/'temp'), TMP=str(run/'temp'),
            WEBVIEW2_USER_DATA_FOLDER=str(run/'webview'),
            PIPER_ESPEAKNG_DATA_DIRECTORY=str(run/'appdata/VoxGolem/models/tts'),
            VOXGOLEM_BENCH_ENTRY='voice-arithmetic-windows',
            VOXGOLEM_BENCH_OUTPUT_ROUTE='wasapi-render-diagnostic')
        home = run/'home'
        env.update(HOMEDRIVE=home.drive, HOMEPATH=str(home)[len(home.drive):])
        # No inherited WebView command-line overrides/debug listeners or host proxies.
        for name in list(env):
            if name.upper().startswith('WEBVIEW2_') and name != 'WEBVIEW2_USER_DATA_FOLDER':
                del env[name]
        system = Path(os.environ['SystemRoot'])
        env['PATH'] = os.pathsep.join(map(str, (roles['binary'].parent, roles['tauri_driver'].parent,
                                               system/'System32', system)))
        env.pop('ALSA_CONFIG_PATH', None)
        for folder in ('appdata', 'localappdata', 'webview', 'temp'):
            private_directory(run/folder, create=False)
        self.private_storage_verified = True
        return env

    def start(self, env, port, native_port):
        self.stage = 'windows_owned_job'
        self.owner = WindowsJob()
        try:
            refuse_preexisting([self.roles[key] for key in ('binary', 'tauri_driver', 'native_driver')])
            command = [str(self.roles['tauri_driver']), '--port', str(port), '--native-port', str(native_port),
                       '--native-driver', str(self.roles['native_driver'])]
            self.stage = 'windows_suspended_driver_assignment'
            self.owner.start(command, env, self.args.output.resolve())
        except BaseException:
            self.last_cleanup = self.owner.shutdown(None)
            self.owner = None
            raise
        return self.owner

    def observe_app(self, pid):
        self.stage = 'windows_app_identity'
        if type(pid) is not int or pid <= 0:
            raise ValueError('Retained benchmark process identity is unavailable')
        self.owner.observe_app(pid, self.roles['binary'])
        self.app_observed = True

    def application_options(self):
        return application_capability(self.roles['binary'])

    def shutdown(self, driver, process):
        if self.owner is None:
            return {'owned_job_retired': False, 'app_exit_before_group_termination_observed': False}
        owner, self.owner = self.owner, None
        self.last_cleanup = owner.shutdown(driver)
        return self.last_cleanup

    def _base_envelope(self):
        args = getattr(self, 'args', None)
        manifest = getattr(self, 'manifest', None)
        return {
            'schema': 4, 'entry': 'voice-arithmetic-windows', 'diagnostic_only': True,
            'overall_status': 'unverified', 'passed': False, 'qualified_for_live': False,
            'offline_profile_qualified': False, 'output_route': 'wasapi-render-diagnostic',
            'os_network_containment': 'unverified', 'acoustic_output': 'unverified',
            'source_manifest_sha256': file_hash(args.windows_manifest) if getattr(args, 'windows_manifest', None) else None,
            'windows_effects_sha256': file_hash(Path(__file__)),
            'native_source_sha256': manifest.get('native_source_sha256') if isinstance(manifest, dict) else None,
            'private_storage_acl_verified': getattr(self, 'private_storage_verified', False),
        }

    def failure_report(self, api, phase, error):
        if self.run_owned and not (self.args.output/'report.json').exists():
            roles = getattr(self, 'roles', None)
            binary = roles.get('binary') if isinstance(roles, dict) else None
            profile_identity = getattr(self, 'profile_identity', None)
            api.private_json(self.args.output/'report.json', {
                **self._base_envelope(),
                'mode': 'offline', 'requested_model': 'custom-luna-none',
                'workflow_completed': False, 'rows': [],
                'binary_sha256': file_hash(binary) if binary else None,
                'harness_sha256': api.harness_identity() if hasattr(api, 'harness_identity') else (file_hash(Path(api.__file__).resolve()) if getattr(api, '__file__', None) else file_hash(Path(__file__).resolve())),
                'profile_identity': profile_identity if profile_identity is not None else {'profile': 'voice-arithmetic-windows-diagnostic-v1', 'contract_sha256': getattr(api, 'voice_contract', None).contract_sha256() if getattr(api, 'voice_contract', None) is not None else ''},
                'cleanup': self.last_cleanup if isinstance(getattr(self, 'last_cleanup', None), dict) else {'owned_job_retired': False, 'app_exit_before_group_termination_observed': False},
                'failure_category': type(error).__name__,
                'session_creation_cause': session_creation_cause(error),
                'stage': phase, 'platform_stage': getattr(self, 'stage', 'unknown'),
                'native_error_code': getattr(error, 'native_error_code', None)})
            api.prune_owned_reports(self.args.output.parent)

    def annotate(self, summary):
        summary.update(
            **self._base_envelope(),
            windows_or_phone_acceptance=False, private_session_bus=False,
            private_process_namespace=False, gpu_runtime_devices_available='unverified',
            hardware_isolation='unverified',
            declared_driver_versions=self.manifest['driver_versions'],
            runtime_driver_compatibility='unverified',
            native_driver_file_version=getattr(self, 'native_driver_version', 'unverified'),
            app_job_and_creation_identity_verified=self.app_observed,
            driver_sha256={key: file_hash(self.roles[key]) for key in ('tauri_driver', 'native_driver')})
        summary['workflow_completed'] = (len(summary['rows']) == 1 and summary['rows'][0].get('passed') is True and
            self.private_storage_verified and self.app_observed and
            summary['cleanup'].get('owned_job_retired') is True and
            summary['cleanup'].get('app_exit_before_group_termination_observed') is True)
