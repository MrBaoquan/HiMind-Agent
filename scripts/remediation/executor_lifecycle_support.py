"""Worker preparation and bounded current-candidate regression support.

Generated output is confined to the current host resource root. No report source
is modified by checks. Cached builds are accepted only with content/toolchain
identity, executable hash and a fresh effective Cargo/Rust configuration probe.
"""
import ctypes
from ctypes import wintypes as W
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import tomllib

WORKSPACE = Path(__file__).resolve().parents[2]

# Fixed public RSA fixture generated in this task's resource with Windows CNG;
# no private bytes were exported. It has no production trust. Both build.rs
# signing consumers use this TEST configuration, never inherited signing paths.
TEST_PUBLIC_KEY = ('-----BEGIN PUBLIC KEY-----\n'
    'MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0RqCOBt4FC5IAd9Ch/1i\n'
    'Ylqx3UIMtshhUaPiFxlpglfc9Xjgab9K44W//VMyA9J+gB1dFvxzCFUjZnprWIPO\n'
    'coFEzG8E4LrQDBatQuKFRcmOIToyeQJjP03YbQnwmNhRjjCAFaZBsOu5O/Vumpxu\n'
    'ZLd+OMl/7JB6lqVQrkz4bfFmQ8AVBKHDuecFDTIsp2Bu4uK+yMgR4H7/IwUpEEWu\n'
    'g6wve3X7bqzjcFC8Cz1vlk9ggO1F49mjXe/KEdGipAOdxoZTHDF1z7xd+yvHXZ5q\n'
    'z+CtekGKI7SykMsdGDvec7DqTr5T16Vag0Oi4P4dCWtKqyZjXWn1UtJA9D8i+KLu\n'
    'iQIDAQAB\n'
    '-----END PUBLIC KEY-----\n')

def initialize_test_build_signing(out):
    directory=out/'build-signing'
    directory.mkdir(exist_ok=True)
    public=directory/'test-public.pem'
    config=directory/'test-build-signing.json'
    # Never repair/overwrite a retained configuration or material mismatch.
    if not public.exists():public.write_text(TEST_PUBLIC_KEY,encoding='ascii',newline='\n')
    if not config.exists():
        write_json(config,{'schema_version':'executor_test_build_signing.v1','classification':'test_only',
            'public_key_file':'test-public.pem','update_key_id':'executor-test-update-v1',
            'extension_key_id':'executor-test-extension-v1','build_channel':'executor-test'})

def build_signing(out):
    directory=(out/'build-signing').resolve(strict=True)
    assert directory.is_relative_to(out.resolve(strict=True)),'test signing ownership mismatch'
    config=directory/'test-build-signing.json'
    value=json.loads(config.read_text(encoding='utf-8'))
    assert value['schema_version']=='executor_test_build_signing.v1' and value['classification']=='test_only','test signing configuration invalid'
    assert value['public_key_file']=='test-public.pem','test signing material ownership mismatch'
    public=(directory/value['public_key_file']).resolve(strict=True)
    assert public.is_relative_to(directory),'test signing material ownership mismatch'
    assert all(isinstance(value[k],str) and re.fullmatch(r'[a-z0-9-]{1,80}',value[k]) for k in
               ['update_key_id','extension_key_id','build_channel']),'test signing configuration invalid'
    return value,public,{'configuration':digest(config),'public_key_material':digest(public)}

def component_hash(value):
    return hashlib.sha256(json.dumps(value,sort_keys=True,separators=(',',':')).encode()).hexdigest()

def identity_differences(expected,actual,component='identity'):
    """Persist only component names and hashes; never raw env/config values."""
    if expected==actual:return []
    if isinstance(expected,dict) and isinstance(actual,dict):
        result=[]
        for key in sorted(expected.keys()|actual.keys()):
            # Dictionary keys are component names, never values. Unknown names
            # are hashed to prevent a malformed input injecting diagnostics.
            name=key if re.fullmatch(r'[A-Za-z0-9_./:-]+',key) else component_hash(key)
            result.extend(identity_differences(expected.get(key),actual.get(key),component+'.'+name))
        return result
    return [{'component':component,'expected_sha256':component_hash(expected),'actual_sha256':component_hash(actual)}]

def require_identity(expected,actual,component='identity'):
    differences=identity_differences(expected,actual,component)
    assert not differences,'identity mismatch: '+json.dumps(differences,sort_keys=True)

def require_artifact(root,path,expected,component):
    actual=Path(path).resolve(strict=True)
    assert actual.is_relative_to(root.resolve(strict=True)),component+' ownership mismatch'
    require_identity(expected,digest(actual),component)
    return actual

def digest(path):
    h=hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda:stream.read(1024*1024),b''): h.update(block)
    return h.hexdigest()

def write_json(path,value):
    temp=Path(str(path)+'.new')
    temp.write_text(json.dumps(value,ensure_ascii=False,indent=2),encoding='utf-8')
    temp.replace(path)

def resource():
    root=Path(os.environ['HIMIND_MISSION_CHECK_ROOT']).resolve(strict=True)
    assert root==Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True),'resource roots disagree'
    assert not root.is_relative_to(WORKSPACE) and not WORKSPACE.is_relative_to(root)
    out=root/'executor-lifecycle-repair'
    out.mkdir(exist_ok=True)
    return root,out

def short_path(path):
    short=ctypes.create_unicode_buffer(32768)
    assert ctypes.windll.kernel32.GetShortPathNameW(str(path),short,len(short)),'task-owned short path unavailable'
    return Path(short.value)

def identity():
    root,out=resource()
    return candidate_identity(WORKSPACE,root,out)

def candidate_identity(workspace,root,out,parent_env=None):
    effective_env=test_environment(root,out,'identity',parent_env=parent_env)
    files=[]
    for directory in ['src','scripts','resources','icons','runtime-profiles','frontend/src','frontend/scripts']:
        files.extend(p for p in (workspace/directory).rglob('*') if p.is_file() and '__pycache__' not in p.parts)
    for name in ['Cargo.toml','Cargo.lock','build.rs','tauri.conf.json','frontend/package.json','frontend/package-lock.json','frontend/vite.config.ts','frontend/tsconfig.json','frontend/index.html','frontend/styles.css']:
        p=workspace/name
        if p.is_file():files.append(p)
    # Resolve literal embedded compile inputs from the source files already
    # in scope, without scanning unrelated directories. OUT_DIR is generated
    # by the bound build script; the resulting executable is hashed separately.
    embedded=[]
    for source in list(files):
        if source.suffix!='.rs':continue
        for relative in re.findall(r'include_(?:str|bytes)!\(\s*"([^"\r\n]+)"',source.read_text(encoding='utf-8')):
            path=(source.parent/relative).resolve(strict=True)
            assert path.is_relative_to(workspace),'embedded compile input outside source workspace'
            embedded.append(path)
    files=sorted(set(files+embedded))
    hashes={p.relative_to(workspace).as_posix():digest(p) for p in sorted(files)}
    versions={}
    for tool,args in [('rustc',['-vV']),('cargo',['-vV'])]:
        process=subprocess.run([tool,*args],cwd=workspace,env=effective_env,capture_output=True,text=True,timeout=4)
        assert process.returncode==0,tool+' identity unavailable'
        versions[tool]=process.stdout.strip()
    linker=subprocess.run(['link.exe','/?'],cwd=workspace,env=effective_env,capture_output=True,text=True,errors='replace',timeout=4)
    linker_version=next((line.strip() for line in (linker.stdout+linker.stderr).splitlines() if 'Linker Version ' in line),None)
    assert linker_version,'MSVC linker identity unavailable'
    versions['msvc_linker']=linker_version
    keys={key for key in effective_env if key.startswith(('CARGO_','RUST','HIMIND_SIGNING_','HIMIND_EXTENSION_SIGNING_'))}
    keys.update(['HIMIND_BUILD_CHANNEL','CC','CXX','AR','CL','LINK','LIB','INCLUDE','VCINSTALLDIR','WindowsSdkDir'])
    # Preserve configuration identity without copying possible registry tokens.
    env={key:hashlib.sha256(effective_env[key].encode()).hexdigest() if key in effective_env else None for key in sorted(keys)}
    # Bind the actual public bytes and configuration, not only the filename.
    value={'files':hashes,'toolchain':versions,'build_environment':env,'build_signing':build_signing(out)[2],
        'build_command':['cargo','test','--locked','--bin','himind-agent','--no-run','--message-format=json']}
    value['candidate_id']=hashlib.sha256(json.dumps(value,sort_keys=True).encode()).hexdigest()
    return value


def configuration(root,out,workspace=None):
    # build.rs watches .git/HEAD, which is absent inside this worktree's .git
    # file. Cargo consequently reruns the expensive main build every time.
    # Resolve the same inherited Cargo configuration in this cwd with a tiny
    # generated package instead; its name preserves per-package overrides.
    workspace=WORKSPACE if workspace is None else workspace
    probe=short_path(out)/'configuration-probe'
    (probe/'src').mkdir(parents=True,exist_ok=True)
    package=tomllib.loads((workspace/'Cargo.toml').read_text(encoding='utf-8'))['package']
    manifest='[package]\nname='+json.dumps(package['name'])+'\nversion='+json.dumps(package['version'])+'\nedition='+json.dumps(package['edition'])+'\n[workspace]\n'
    for path,content in [(probe/'Cargo.toml',manifest),(probe/'src/main.rs','fn main() {}\n')]:
        if not path.is_file() or path.read_text(encoding='utf-8')!=content:path.write_text(content,encoding='utf-8')
    command=['cargo','rustc','--manifest-path',str(probe/'Cargo.toml'),'--offline','--profile','test','-vv','--','--print=cfg']
    record,output=run(root,out,'effective-build-configuration',command,4,cwd=short_path(workspace))
    assert record['exit_code']==0 and not record['timed_out'],'effective build configuration unavailable: '+record['evidence']
    commands=[];cfg=[]
    for line in output.splitlines():
        if 'Running `' in line and ' --crate-name himind_agent ' in line:
            # Cargo prints process environment before the executable. Its PATH
            # and process-local values are not compiler flags; do not persist
            # potentially sensitive environment in the identity document.
            match=re.search(r'(?:"[^"\r\n]*rustc\.exe"|[^\s]*rustc\.exe) --crate-name himind_agent .*',line)
            assert match,'effective rustc command missing'
            commands.append(match[0].rstrip('`'))
        elif line and not line[0].isspace() and (line=='windows' or line=='debug_assertions' or '=' in line):cfg.append(line)
    assert len(commands)==1 and cfg,'configuration probe did not execute rustc'
    return record,{'rustc_commands':commands,'cfg':sorted(cfg),'probe_manifest_sha256':digest(probe/'Cargo.toml')}

def test_environment(root,out,label,parent_env=None):
    # Reproduce the prepared Cargo linker configuration, without compiling it.
    generated=root/'executor'
    generated.mkdir(exist_ok=True)
    short=ctypes.create_unicode_buffer(32768)
    assert ctypes.windll.kernel32.GetShortPathNameW(str(generated),short,len(short))
    generated_short=Path(short.value)
    # SQLite and rustc's direct link.exe fixture builds still require short
    # Windows paths. This is the same task-owned directory, not another root.
    # Every execution gets a new caller home. Durable run IDs intentionally
    # reject reentry; test setup must never reuse a previous live run's meter.
    temp=generated_short/('x'+format(time.time_ns(),'x')[-9:])
    temp.mkdir(exist_ok=True)
    # Drop inherited signing context before touching any file. In particular,
    # no production key path is opened, expanded, copied or printed here.
    parent=os.environ if parent_env is None else parent_env
    parent={k:v for k,v in parent.items() if not k.startswith(('HIMIND_SIGNING_','HIMIND_EXTENSION_SIGNING_'))}
    env=dict(parent,CARGO_HOME=str(generated_short/'cargo'),CARGO_NET_OFFLINE='false',CARGO_TARGET_DIR=str(generated_short/'t'),RUST_MIN_STACK='16777216',TEMP=str(temp),TMP=str(temp),
             CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=str(generated_short/'linkwrap.exe'),
             EXECUTOR_LONG_ROOT=str(generated),EXECUTOR_SHORT_ROOT=str(generated_short),
             HIMIND_AGENT_HOME=str(temp/'home'),GIT_CONFIG_COUNT='1',GIT_CONFIG_KEY_0='core.longpaths',GIT_CONFIG_VALUE_0='true')
    env['HIMIND_EXECUTOR_TEST_RSA_KEY']=str(out/'test-signing-key.pem')
    config,public,_=build_signing(out)
    env.update(HIMIND_SIGNING_PUBLIC_KEY_PATH=str(public),HIMIND_SIGNING_KEY_ID=config['update_key_id'],
        HIMIND_EXTENSION_SIGNING_PUBLIC_KEY_PATH=str(public),HIMIND_EXTENSION_SIGNING_KEY_ID=config['extension_key_id'],
        HIMIND_BUILD_CHANNEL=config['build_channel'])
    return env

class OwnedProcess:
    """Assign a suspended check before any test code runs; kill only this job."""
    def __init__(self,command,env,stream,cwd=WORKSPACE):
        k=ctypes.WinDLL('kernel32',use_last_error=True)
        self.k=k
        k.CreateJobObjectW.restype=W.HANDLE
        k.CreateJobObjectW.argtypes=[W.LPVOID,W.LPCWSTR]
        k.CloseHandle.argtypes=[W.HANDLE]
        k.SetInformationJobObject.argtypes=[W.HANDLE,ctypes.c_int,W.LPVOID,W.DWORD]
        k.AssignProcessToJobObject.argtypes=[W.HANDLE,W.HANDLE]
        k.TerminateJobObject.argtypes=[W.HANDLE,W.UINT]
        class Basic(ctypes.Structure):
            _fields_=[('process_time',ctypes.c_longlong),('job_time',ctypes.c_longlong),('flags',W.DWORD),('min',ctypes.c_size_t),('max',ctypes.c_size_t),('active',W.DWORD),('affinity',ctypes.c_size_t),('priority',W.DWORD),('scheduling',W.DWORD)]
        class Limits(ctypes.Structure):
            _fields_=[('basic',Basic),('io',ctypes.c_ulonglong*6),('memory',ctypes.c_size_t*4)]
        self.job=k.CreateJobObjectW(None,None)
        if not self.job:raise ctypes.WinError(ctypes.get_last_error())
        self.process=None
        try:
            limits=Limits();limits.basic.flags=0x2000
            if not k.SetInformationJobObject(self.job,9,ctypes.byref(limits),ctypes.sizeof(limits)):raise ctypes.WinError(ctypes.get_last_error())
            self.process=subprocess.Popen(command,cwd=cwd,env=env,stdout=stream,stderr=subprocess.STDOUT,creationflags=0x08000004)
            if not k.AssignProcessToJobObject(self.job,int(self.process._handle)):raise ctypes.WinError(ctypes.get_last_error())
            class Entry(ctypes.Structure):
                _fields_=[('size',W.DWORD),('usage',W.DWORD),('id',W.DWORD),('owner',W.DWORD),('base',W.LONG),('delta',W.LONG),('flags',W.DWORD)]
            k.CreateToolhelp32Snapshot.argtypes=[W.DWORD,W.DWORD];k.CreateToolhelp32Snapshot.restype=W.HANDLE
            k.Thread32First.argtypes=[W.HANDLE,ctypes.POINTER(Entry)];k.Thread32Next.argtypes=[W.HANDLE,ctypes.POINTER(Entry)]
            snapshot=k.CreateToolhelp32Snapshot(4,0)
            if snapshot==ctypes.c_void_p(-1).value:raise ctypes.WinError(ctypes.get_last_error())
            entry=Entry();entry.size=ctypes.sizeof(entry);threads=[]
            try:
                more=k.Thread32First(snapshot,ctypes.byref(entry))
                while more:
                    if entry.owner==self.process.pid:threads.append(entry.id)
                    more=k.Thread32Next(snapshot,ctypes.byref(entry))
            finally:k.CloseHandle(snapshot)
            assert len(threads)==1,'suspended check primary thread missing or ambiguous'
            k.OpenThread.argtypes=[W.DWORD,W.BOOL,W.DWORD];k.OpenThread.restype=W.HANDLE
            k.ResumeThread.argtypes=[W.HANDLE];k.ResumeThread.restype=W.DWORD
            thread=k.OpenThread(2,False,threads[0])
            if not thread:raise ctypes.WinError(ctypes.get_last_error())
            try:assert k.ResumeThread(thread)==1,'check resume failed'
            finally:k.CloseHandle(thread)
        except BaseException:
            self.close()
            raise
    def close(self):
        if self.job:
            self.k.TerminateJobObject(self.job,1)
            if self.process:
                if self.process.poll() is None:self.process.kill()
                self.process.wait(timeout=5)
            self.k.CloseHandle(self.job);self.job=None

def run(root,out,label,command,timeout,env=None,cwd=WORKSPACE):
    log=out/(label+'-'+str(time.time_ns())+'.log')
    started=time.monotonic();timed_out=False;error=None;exit_code=-1
    try:
        with log.open('w',encoding='utf-8') as stream:
            owned=OwnedProcess(command,env or test_environment(root,out,label),stream,cwd=cwd)
            try:
                try:exit_code=int(owned.process.wait(timeout=max(.1,timeout)))
                except subprocess.TimeoutExpired:timed_out=True
            finally:owned.close()
    except Exception as failure:
        error=str(failure)
        with log.open('a',encoding='utf-8') as stream:stream.write('\nrunner failure: '+error)
    record={'name':label,'command':subprocess.list2cmdline(command),'argv':command,'exit_code':exit_code,
            'elapsed_ms':round((time.monotonic()-started)*1000),'timed_out':timed_out,
            'evidence':str(log),'sha256':digest(log),'test_count':0}
    if error:record['error']=error
    return record,log.read_text(encoding='utf-8',errors='replace')
