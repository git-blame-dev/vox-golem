"""Windows-only effects for the shared offline voice diagnostic (stdlib + Selenium).

No namespace/network/hardware isolation claim. Win32 structures/signatures follow
Microsoft Learn: CreateProcessW, JOBOBJECT_{BASIC,EXTENDED}_LIMIT_INFORMATION,
JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, IsProcessInJob and GetProcessTimes.
No breakaway flags, inherited handles, global process-name termination or install.
"""
import ctypes as C
import hashlib
import ipaddress
import json
import os
from pathlib import Path, PurePosixPath
import re
import struct
import subprocess
import sys
import time
import tomllib
import urllib.request
from urllib.parse import urlsplit

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
    if not is_win32 or getattr(args, 'live', False) or getattr(args, 'entry', None) != 'typed-speech':
        raise ValueError('Windows supports only native offline Neural typed-speech diagnostics')
    if (
            getattr(args, 'cases', ['arithmetic']) != ['arithmetic'] or
            getattr(args, 'wake_phrase', None) is not None or
            any(getattr(args, key, False) for key in ('extended', 'shared_suite', 'wake_diagnostics',
                'event_loop_diagnostic', 'driver_free_diagnostic', 'hold_native_observer')) or
            getattr(args, 'poll_interval', 0.1) != 0.1):
        raise ValueError('Typed speech requires one offline arithmetic case without voice assets or probes')
    if getattr(args, 'windows_manifest', None) is None:
        raise ValueError('Windows requires a verified schema-2 typed-speech manifest')


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


def application_capability(binary, user_data_folder=None):
    # Same Selenium/Tauri protocol as Linux, not a second WebDriver client.
    value = {'application': str(binary.resolve())}
    if user_data_folder is not None:
        # EdgeDriver otherwise replaces WEBVIEW2_USER_DATA_FOLDER with its own
        # temporary profile, which the native exact-folder guard rejects.
        value['webviewOptions'] = {'userDataFolder': str(user_data_folder.resolve())}
    return value


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


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Duplicate descriptor or manifest key')
        result[key] = value
    return result


def load_manifest(path, profile, entry='typed-speech'):
    if entry != 'typed-speech':
        raise ValueError('Desktop synthetic-voice arithmetic profile retired')
    reject_reparse(path)
    if not path.is_file() or path.stat().st_size > 256*1024:
        raise ValueError('Bounded Windows staging manifest required')
    value = json.loads(path.read_text(encoding='utf-8'),
                       object_pairs_hook=unique_object)
    if (not isinstance(value, dict) or type(value.get('schema')) is not int or value.get('schema') != 2 or
            value.get('entry') != 'typed-speech-windows' or value.get('contract_sha256') != profile.contract_sha256 or
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
    required = {'binary', 'tauri_driver', 'native_driver', 'config'}
    if set(roles) != required:
        raise ValueError('Windows manifest roles incomplete')
    resolved = {}
    for role, name in roles.items():
        target = bundle_path(root, name)
        if name not in files:
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
    neural_configuration(resolved['config'])
    return value, resolved


def valid_neural_id(value):
    return isinstance(value, str) and re.fullmatch(r'[A-Za-z0-9_.-]{1,256}', value) is not None


def neural_configuration(path):
    """Synthetic source only: no credentials, launch commands or speech assets."""
    if path.stat().st_size > 64*1024:
        raise ValueError('Neural configuration exceeds bounds')
    config = tomllib.loads(path.read_text(encoding='utf-8'))
    tts = config.get('tts')
    if (set(config) != {'tts'} or not isinstance(tts, dict) or
            set(tts) != {'endpoint', 'voice', 'start_command'} or
            tts['start_command'] != [] or
            not valid_neural_id(tts['voice']) or not isinstance(tts['endpoint'], str) or
            len(tts['endpoint']) > 2048 or any(ord(c) <= 32 or ord(c) >= 127 for c in tts['endpoint'])):
        raise ValueError('Synthetic explicit Neural-only configuration required')
    try:
        url = urlsplit(tts['endpoint'])
        if (url.scheme != 'http' or not ipaddress.ip_address(url.hostname).is_loopback or
                url.username is not None or url.password is not None or url.query or url.fragment or
                url.path not in ('', '/') or url.port is None or not 1 <= url.port <= 65535 or
                '?' in tts['endpoint'] or '#' in tts['endpoint']):
            raise ValueError()
    except (ValueError, TypeError):
        raise ValueError('Literal loopback HTTP Neural endpoint with explicit port required') from None
    return tts


def neural_descriptor(value, requested_voice):
    """Validate the bounded existing-service descriptor; retain public identity only."""
    if (not isinstance(value, dict) or type(value.get('protocol_version')) is not int or
            value['protocol_version'] != 1 or value.get('ready') is not True or
            not valid_neural_id(value.get('backend')) or not valid_neural_id(value.get('default_voice')) or
            type(value.get('sample_rate_hz')) is not int or not 1 <= value['sample_rate_hz'] <= 96000 or
            type(value.get('channels')) is not int or value['channels'] != 1 or
            type(value.get('incremental_audio')) is not bool or
            not isinstance(value.get('voices'), list) or not 1 <= len(value['voices']) <= 32):
        raise ValueError('Neural descriptor invalid or not ready')
    identities, selected = set(), None
    for voice in value['voices']:
        if (not isinstance(voice, dict) or not valid_neural_id(voice.get('id')) or
                not isinstance(voice.get('name'), str) or not 1 <= len(voice['name'].encode()) <= 256 or
                any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in voice['name']) or
                not isinstance(voice.get('aliases', []), list) or len(voice.get('aliases', [])) > 32 or
                type(voice.get('default', False)) is not bool):
            raise ValueError('Neural voice descriptor invalid')
        names = [voice['id'], *voice.get('aliases', [])]
        for name in names:
            if not valid_neural_id(name) or name in identities:
                raise ValueError('Neural voice identity invalid or duplicated')
            identities.add(name)
        if requested_voice in names:
            selected = voice['id']
    if value['default_voice'] not in identities or selected is None:
        raise ValueError('Neural selected/default voice unavailable')
    return dict(engine='neural', protocol_version=1, backend=value['backend'], voice=selected,
                sample_rate_hz=value['sample_rate_hz'], incremental_audio=value['incremental_audio'])


def probe_neural_service(config):
    # GET only: no proxy, redirects, retries, sessions or lifecycle control.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    url = config['endpoint'].rstrip('/')+'/v1/synthesis/capabilities'
    with opener.open(url, timeout=5) as response:
        if response.status != 200:
            raise ValueError('Existing Neural service is unavailable')
        raw = response.read(64*1024+1)
    if len(raw) > 64*1024:
        raise ValueError('Neural descriptor exceeds bounds')
    return neural_descriptor(json.loads(raw, object_pairs_hook=unique_object), config['voice'])


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
            'profile': 'typed-speech-windows-diagnostic-v1',
            'contract_sha256': ''
        }

    def prepare(self, profile, api):
        self.profile_identity = dict(profile='typed-speech-windows-diagnostic-v1',
                                     contract_sha256=profile.contract_sha256,
                                     implementation_sha256=profile.implementation_sha256)
        args = self.args
        value, roles = load_manifest(args.windows_manifest, profile, 'typed-speech')
        if (args.binary.resolve() != roles['binary'] or args.config.resolve() != roles['config'] or
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
        self.stage = 'windows_neural_descriptor'
        speech = probe_neural_service(neural_configuration(roles['config']))
        identity = dict(profile='typed-speech-windows-diagnostic-v1',
            contract_sha256=profile.contract_sha256, implementation_sha256=profile.implementation_sha256,
            speech_identity=speech, external_shared_service_lifecycle='unverified')
        self.profile_identity = identity
        return {}, speech, identity

    def environment(self, endpoint, api):
        self.stage = 'windows_private_environment'
        args, roles, run = self.args, self.roles, self.args.output.resolve()
        system_keys = {'SYSTEMROOT', 'WINDIR', 'SYSTEMDRIVE', 'COMSPEC', 'PROGRAMFILES',
            'PROGRAMFILES(X86)', 'PROGRAMW6432', 'PROGRAMDATA', 'NUMBER_OF_PROCESSORS',
            'PROCESSOR_ARCHITECTURE', 'PROCESSOR_IDENTIFIER', 'PROCESSOR_LEVEL', 'PROCESSOR_REVISION'}
        source = {key: value for key, value in os.environ.items() if key.upper() in system_keys}
        env = api.isolated_environment(run, roles['config'], endpoint,
            entry='typed-speech', output_route='windows-diagnostic',
            base_environment=source)
        env.update(APPDATA=str(run/'appdata'), LOCALAPPDATA=str(run/'localappdata'),
            USERPROFILE=str(run/'home'), TEMP=str(run/'temp'), TMP=str(run/'temp'),
            WEBVIEW2_USER_DATA_FOLDER=str(run/'webview'),
            VOXGOLEM_BENCH_ENTRY='typed-speech-windows',
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
        return application_capability(self.roles['binary'], self.args.output/'webview')

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
            'external_shared_service_lifecycle': 'unverified', 'live_calls': 0,
            'schema': 4, 'entry': 'typed-speech-windows', 'diagnostic_only': True,
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
                'profile_identity': profile_identity if profile_identity is not None else {'profile': 'typed-speech-windows-diagnostic-v1', 'contract_sha256': getattr(api, 'voice_contract', None).contract_sha256() if getattr(api, 'voice_contract', None) is not None else ''},
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
